//! Per-system latency-driven Time Dilation (TiDi) autoscaler with hysteresis and
//! synchronized advance-notice transitions (`#1081`).
//!
//! Design invariants:
//! - Control window is [`TIDI_CONTROL_WINDOW_SECS`] (`1.0 s` / [`TIDI_TARGET_WINDOW_MS`] `1000.0 ms`).
//! - Engages when tick lateness exceeds [`TIDI_ENGAGE_LATENESS_MS`] (`20.0 ms`).
//! - Target factor follows `clamp(target / (target + (actual - engage)), 0.1, 1.0)`.
//! - Tightening schedules immediately (`reason = "tighten"`) unless pinned/disabled or held.
//! - Relaxing requires [`TIDI_RELAX_CONFIRM_WINDOWS`] (`2`) consecutive low-load windows
//!   (`<= TIDI_RELAX_THRESHOLD_MS`, i.e. `engage - slack = 15.0 ms`); the first window reports
//!   `"await-relax-confirmation"`, and any rising load resets the counter and cancels pending relax.
//! - Minimum hold [`TIDI_MIN_HOLD_SECS`] (`5.0 s`) prevents rapid back-to-back factor oscillations.
//! - Synchronized switch announces a [`PendingTidiTransition`] with [`TIDI_ADVANCE_NOTICE_SECS`] (`2.0 s`)
//!   advance notice and applies at a Destiny stamp boundary so server and clients switch together.

#![forbid(unsafe_code)]

use eve_sde::SolarSystemId;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;

/// Control-loop evaluation window duration in wall-clock seconds (`1.0 s`).
pub const TIDI_CONTROL_WINDOW_SECS: f64 = 1.0;

/// Target control-loop window duration in milliseconds (`1000.0 ms`), matching the denominator
/// term `target` in `factor = clamp(target / (target + (actual - engage)), 0.1, 1.0)`.
pub const TIDI_TARGET_WINDOW_MS: f64 = 1_000.0;

/// Hysteresis slack below [`TIDI_ENGAGE_LATENESS_MS`] in milliseconds (`5.0 ms`).
/// Low-load relaxation only counts windows whose lateness is at or below
/// `TIDI_ENGAGE_LATENESS_MS - TIDI_LATENESS_SLACK_MS` (`15.0 ms`).
pub const TIDI_LATENESS_SLACK_MS: f64 = 5.0;

/// Tick lateness threshold in milliseconds (`20.0 ms`) above which time dilation engages or tightens.
pub const TIDI_ENGAGE_LATENESS_MS: f64 = 20.0;

/// Derived low-load relaxation threshold in milliseconds (`15.0 ms`).
pub const TIDI_RELAX_THRESHOLD_MS: f64 = TIDI_ENGAGE_LATENESS_MS - TIDI_LATENESS_SLACK_MS;

/// Minimum supported Time Dilation factor (`0.10`, i.e. 10% real-time speed).
pub const TIDI_MIN_FACTOR: f64 = 0.10;

/// Maximum / undilated Time Dilation factor (`1.00`, i.e. 100% real-time speed).
pub const TIDI_MAX_FACTOR: f64 = 1.00;

/// Minimum wall-clock hold time in seconds between applied TiDi factor transitions (`5.0 s`).
pub const TIDI_MIN_HOLD_SECS: f64 = 5.0;

/// Number of consecutive low-load control windows required before relaxing TiDi (`2`).
pub const TIDI_RELAX_CONFIRM_WINDOWS: u32 = 2;

/// Advance notice window in wall-clock seconds before a synchronized TiDi switch takes effect (`2.0 s`).
pub const TIDI_ADVANCE_NOTICE_SECS: f64 = 2.0;

/// Advance notice stamp offset aligned to 1 Hz Destiny stamps (`2` stamps).
pub const TIDI_ADVANCE_NOTICE_STAMPS: u64 = 2;

/// Minimum factor delta required to schedule or apply a transition; smaller changes are skipped.
pub const TIDI_MIN_CHANGE_DELTA: f64 = 0.005;

/// Maximum retained decision log entries per solar system autoscaler.
pub const TIDI_MAX_DECISION_LOG_ENTRIES: usize = 64;

/// Canonical reason codes recorded by [`SystemTidiAutoscaler`] and exposed via `/tidi status`
/// and ops snapshots (`#1081`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TidiTransitionReason {
    Normal,
    Tighten,
    AwaitRelaxConfirmation,
    Relax,
    MinHoldActive,
    AlreadyAtTarget,
    ManualPin,
    ManualClear,
    Disabled,
}

impl TidiTransitionReason {
    /// Returns the wire/log string identifier for this transition reason.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::Tighten => "tighten",
            Self::AwaitRelaxConfirmation => "await-relax-confirmation",
            Self::Relax => "relax",
            Self::MinHoldActive => "min-hold-active",
            Self::AlreadyAtTarget => "already-at-target",
            Self::ManualPin => "manual-pin",
            Self::ManualClear => "manual-clear",
            Self::Disabled => "disabled",
        }
    }
}

/// Announced synchronized TiDi factor transition awaiting its advance-notice stamp boundary.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PendingTidiTransition {
    pub from_factor: f64,
    pub to_factor: f64,
    pub reason: String,
    pub announced_at_secs: f64,
    pub apply_at_secs: f64,
    pub target_stamp: u64,
    pub lateness_ms: f64,
}

/// Outcome returned when a scheduled transition crosses its stamp boundary and applies.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AppliedTidiTransition {
    pub old_factor: f64,
    pub new_factor: f64,
    pub reason: String,
    pub applied_at_secs: f64,
    pub applied_stamp: u64,
    pub lateness_ms: f64,
}

/// Single control-window decision entry retained for diagnostics, `/tidi status`, and `#1077` `[PacketPerf]`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TidiDecisionRecord {
    pub timestamp_secs: f64,
    pub lateness_ms: f64,
    pub packet_perf_latency_ms: Option<f64>,
    pub current_factor: f64,
    pub target_factor: f64,
    pub reason: String,
    pub pending: Option<PendingTidiTransition>,
}

/// Point-in-time observability snapshot for one solar system's TiDi autoscaler.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SystemTidiSnapshot {
    pub solar_system_id: SolarSystemId,
    pub enabled: bool,
    pub pinned: bool,
    pub factor: f64,
    pub lateness_ms: f64,
    pub packet_perf_latency_ms: Option<f64>,
    pub reason: String,
    pub pending_transition: Option<PendingTidiTransition>,
    pub consecutive_low_load_windows: u32,
    pub last_transition_secs: f64,
    pub decision_log: Vec<TidiDecisionRecord>,
}

/// Cluster-wide TiDi observability snapshot.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClusterTidiSnapshot {
    pub autoscaler_enabled: bool,
    pub node_load_auto_tidi_enabled: bool,
    pub systems: Vec<SystemTidiSnapshot>,
}

/// Result of evaluating one 1-second autoscaler control window.
#[derive(Debug, Clone, PartialEq)]
pub struct AutoscalerWindowOutcome {
    pub announced: Option<PendingTidiTransition>,
    pub applied: Option<AppliedTidiTransition>,
    pub reason: String,
    pub target_factor: f64,
}

/// Computes the target Time Dilation factor from observed control-window tick lateness in ms:
/// `factor = clamp(target / (target + (actual - engage)), 0.1, 1.0)`.
///
/// Lateness values at or below [`TIDI_ENGAGE_LATENESS_MS`] (`20.0 ms`) and non-finite inputs
/// return [`TIDI_MAX_FACTOR`] (`1.0`).
pub fn compute_tidi_factor_from_lateness(lateness_ms: f64) -> f64 {
    if !lateness_ms.is_finite() || lateness_ms <= TIDI_ENGAGE_LATENESS_MS {
        return TIDI_MAX_FACTOR;
    }
    let excess = (lateness_ms - TIDI_ENGAGE_LATENESS_MS).max(0.0);
    let factor = TIDI_TARGET_WINDOW_MS / (TIDI_TARGET_WINDOW_MS + excess);
    if factor.is_finite() {
        factor.clamp(TIDI_MIN_FACTOR, TIDI_MAX_FACTOR)
    } else {
        TIDI_MAX_FACTOR
    }
}

/// Clamps any user/calculated TiDi factor cleanly into `[TIDI_MIN_FACTOR, TIDI_MAX_FACTOR]`.
pub fn clamp_tidi_factor(factor: f64) -> f64 {
    if factor.is_finite() {
        factor.clamp(TIDI_MIN_FACTOR, TIDI_MAX_FACTOR)
    } else {
        TIDI_MAX_FACTOR
    }
}

/// Per-solar-system TiDi autoscaler state machine.
#[derive(Debug, Clone)]
pub struct SystemTidiAutoscaler {
    pub solar_system_id: SolarSystemId,
    pub enabled: bool,
    pub pinned_factor: Option<f64>,
    pub current_factor: f64,
    pub last_lateness_ms: f64,
    pub last_packet_perf_latency_ms: Option<f64>,
    pub last_reason: String,
    pub consecutive_low_load_windows: u32,
    pub last_transition_secs: f64,
    pub has_transitioned: bool,
    pub pending_transition: Option<PendingTidiTransition>,
    decision_log: VecDeque<TidiDecisionRecord>,
}

impl SystemTidiAutoscaler {
    /// Creates a new per-system autoscaler, enabled by default at `factor = 1.0`.
    pub fn new(solar_system_id: SolarSystemId, enabled: bool) -> Self {
        Self {
            solar_system_id,
            enabled,
            pinned_factor: None,
            current_factor: TIDI_MAX_FACTOR,
            last_lateness_ms: 0.0,
            last_packet_perf_latency_ms: None,
            last_reason: TidiTransitionReason::Normal.as_str().to_string(),
            consecutive_low_load_windows: 0,
            last_transition_secs: -TIDI_MIN_HOLD_SECS,
            has_transitioned: false,
            pending_transition: None,
            decision_log: VecDeque::with_capacity(TIDI_MAX_DECISION_LOG_ENTRIES),
        }
    }

    /// Records a `[PacketPerf]` / `#1077` latency observation into the autoscaler decision state.
    pub fn record_packet_perf_latency(&mut self, now_secs: f64, latency_ms: f64) {
        if !latency_ms.is_finite() {
            return;
        }
        let clamped = latency_ms.max(0.0);
        self.last_packet_perf_latency_ms = Some(clamped);
        let reason = if self.pending_transition.is_some() {
            self.last_reason.clone()
        } else if clamped > TIDI_ENGAGE_LATENESS_MS {
            "packet-perf-sample".to_string()
        } else {
            self.last_reason.clone()
        };
        self.push_decision_log(
            now_secs,
            self.last_lateness_ms,
            Some(clamped),
            self.current_factor,
            reason,
        );
    }

    /// Manually pins the system to `factor` (`/tidi <f>`), cancelling any pending transition.
    pub fn pin_manual(&mut self, now_secs: f64, factor: f64) -> (f64, f64) {
        let old = self.current_factor;
        let clamped = clamp_tidi_factor(factor);
        self.pinned_factor = Some(clamped);
        self.current_factor = clamped;
        self.pending_transition = None;
        self.consecutive_low_load_windows = 0;
        self.last_reason = TidiTransitionReason::ManualPin.as_str().to_string();
        if (clamped - old).abs() >= TIDI_MIN_CHANGE_DELTA {
            self.last_transition_secs = now_secs;
            self.has_transitioned = true;
        }
        self.push_decision_log(
            now_secs,
            self.last_lateness_ms,
            self.last_packet_perf_latency_ms,
            clamped,
            self.last_reason.clone(),
        );
        (old, clamped)
    }

    /// Clears manual pinning and pending transitions and deterministically restores `1.0`.
    pub fn clear_manual(&mut self, now_secs: f64) -> (f64, f64) {
        let old = self.current_factor;
        self.pinned_factor = None;
        self.pending_transition = None;
        self.consecutive_low_load_windows = 0;
        self.current_factor = TIDI_MAX_FACTOR;
        self.last_reason = TidiTransitionReason::ManualClear.as_str().to_string();
        if (TIDI_MAX_FACTOR - old).abs() >= TIDI_MIN_CHANGE_DELTA {
            self.last_transition_secs = now_secs;
            self.has_transitioned = true;
        }
        self.push_decision_log(
            now_secs,
            self.last_lateness_ms,
            self.last_packet_perf_latency_ms,
            TIDI_MAX_FACTOR,
            self.last_reason.clone(),
        );
        (old, TIDI_MAX_FACTOR)
    }

    /// Enables or disables automatic scaling for this system (`/autotidi on|off`).
    pub fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
        if !enabled {
            self.pending_transition = None;
            self.consecutive_low_load_windows = 0;
            self.last_reason = TidiTransitionReason::Disabled.as_str().to_string();
        } else if self.last_reason == TidiTransitionReason::Disabled.as_str() {
            self.last_reason = TidiTransitionReason::Normal.as_str().to_string();
        }
    }

    /// Schedules a synchronized transition with `advance_notice_secs` (~2 s) applied at a stamp
    /// boundary. Systems already at `target_factor` (or already pending the same target) are skipped.
    pub fn schedule_transition(
        &mut self,
        now_secs: f64,
        current_stamp: u64,
        target_factor: f64,
        reason: &str,
        advance_notice_secs: f64,
    ) -> Option<PendingTidiTransition> {
        let target = clamp_tidi_factor(target_factor);
        if (target - self.current_factor).abs() < TIDI_MIN_CHANGE_DELTA {
            self.last_reason = TidiTransitionReason::AlreadyAtTarget.as_str().to_string();
            return None;
        }
        if let Some(existing) = &self.pending_transition {
            if (existing.to_factor - target).abs() < TIDI_MIN_CHANGE_DELTA {
                return None;
            }
        }
        let notice = if advance_notice_secs.is_finite() {
            advance_notice_secs.max(0.0)
        } else {
            TIDI_ADVANCE_NOTICE_SECS
        };
        let stamp_offset = (notice.ceil() as u64).max(1);
        let pending = PendingTidiTransition {
            from_factor: self.current_factor,
            to_factor: target,
            reason: reason.to_string(),
            announced_at_secs: now_secs,
            apply_at_secs: now_secs + notice,
            target_stamp: current_stamp.saturating_add(stamp_offset),
            lateness_ms: self.last_lateness_ms,
        };
        self.pending_transition = Some(pending.clone());
        self.last_reason = reason.to_string();
        Some(pending)
    }

    /// Polls and applies a pending transition when both `now_secs >= apply_at_secs` and
    /// `current_stamp >= target_stamp`. Skips emission if factor already matches target.
    pub fn poll_due_transition(
        &mut self,
        now_secs: f64,
        current_stamp: u64,
    ) -> Option<AppliedTidiTransition> {
        let pending = self.pending_transition.as_ref()?;
        if now_secs + 1e-9 < pending.apply_at_secs || current_stamp < pending.target_stamp {
            return None;
        }
        let pending = self.pending_transition.take()?;
        let old = self.current_factor;
        if (pending.to_factor - old).abs() < TIDI_MIN_CHANGE_DELTA {
            self.last_reason = TidiTransitionReason::AlreadyAtTarget.as_str().to_string();
            return None;
        }
        self.current_factor = pending.to_factor;
        self.last_transition_secs = now_secs;
        self.has_transitioned = true;
        self.last_reason = pending.reason.clone();
        self.consecutive_low_load_windows = 0;
        let applied = AppliedTidiTransition {
            old_factor: old,
            new_factor: pending.to_factor,
            reason: pending.reason,
            applied_at_secs: now_secs,
            applied_stamp: current_stamp,
            lateness_ms: self.last_lateness_ms,
        };
        self.push_decision_log(
            now_secs,
            self.last_lateness_ms,
            self.last_packet_perf_latency_ms,
            pending.to_factor,
            self.last_reason.clone(),
        );
        Some(applied)
    }

    /// Evaluates one `1.0 s` control window of tick lateness (plus optional `[PacketPerf]` latency)
    /// and updates pending/applied synchronized switches per hysteresis and hold rules.
    pub fn observe_window(
        &mut self,
        now_secs: f64,
        current_stamp: u64,
        lateness_ms: f64,
        packet_perf_latency_ms: Option<f64>,
    ) -> AutoscalerWindowOutcome {
        let safe_lateness = if lateness_ms.is_finite() {
            lateness_ms.max(0.0)
        } else {
            0.0
        };
        self.last_lateness_ms = safe_lateness;
        if let Some(perf) = packet_perf_latency_ms.filter(|v| v.is_finite()) {
            self.last_packet_perf_latency_ms = Some(perf.max(0.0));
        }

        // First apply any scheduled transition whose advance-notice stamp boundary arrived.
        let mut applied = self.poll_due_transition(now_secs, current_stamp);

        if let Some(pin) = self.pinned_factor {
            self.pending_transition = None;
            self.consecutive_low_load_windows = 0;
            self.last_reason = TidiTransitionReason::ManualPin.as_str().to_string();
            self.push_decision_log(
                now_secs,
                safe_lateness,
                self.last_packet_perf_latency_ms,
                pin,
                self.last_reason.clone(),
            );
            return AutoscalerWindowOutcome {
                announced: None,
                applied,
                reason: self.last_reason.clone(),
                target_factor: pin,
            };
        }

        if !self.enabled {
            self.pending_transition = None;
            self.consecutive_low_load_windows = 0;
            self.last_reason = TidiTransitionReason::Disabled.as_str().to_string();
            self.push_decision_log(
                now_secs,
                safe_lateness,
                self.last_packet_perf_latency_ms,
                self.current_factor,
                self.last_reason.clone(),
            );
            return AutoscalerWindowOutcome {
                announced: None,
                applied,
                reason: self.last_reason.clone(),
                target_factor: self.current_factor,
            };
        }

        let hold_elapsed = !self.has_transitioned
            || (now_secs - self.last_transition_secs) + 1e-9 >= TIDI_MIN_HOLD_SECS;
        let mut announced: Option<PendingTidiTransition> = None;
        let mut target_factor = self.current_factor;

        if safe_lateness > TIDI_ENGAGE_LATENESS_MS {
            // Rising/high load: reset low-load confirmation streak and abort any pending relax.
            self.consecutive_low_load_windows = 0;
            if self
                .pending_transition
                .as_ref()
                .is_some_and(|p| p.to_factor > self.current_factor + TIDI_MIN_CHANGE_DELTA)
            {
                self.pending_transition = None;
            }
            let desired = compute_tidi_factor_from_lateness(safe_lateness);
            target_factor = desired;
            if desired + TIDI_MIN_CHANGE_DELTA < self.current_factor {
                if hold_elapsed {
                    announced = self.schedule_transition(
                        now_secs,
                        current_stamp,
                        desired,
                        TidiTransitionReason::Tighten.as_str(),
                        TIDI_ADVANCE_NOTICE_SECS,
                    );
                    if announced.is_some() {
                        self.last_reason = TidiTransitionReason::Tighten.as_str().to_string();
                    }
                } else {
                    self.last_reason = TidiTransitionReason::MinHoldActive.as_str().to_string();
                }
            } else if self.pending_transition.is_none() && applied.is_none() {
                self.last_reason = TidiTransitionReason::AlreadyAtTarget.as_str().to_string();
            }
        } else if safe_lateness <= TIDI_RELAX_THRESHOLD_MS {
            if self.current_factor < TIDI_MAX_FACTOR - TIDI_MIN_CHANGE_DELTA {
                self.consecutive_low_load_windows =
                    self.consecutive_low_load_windows.saturating_add(1);
                target_factor = TIDI_MAX_FACTOR;
                if self.consecutive_low_load_windows < TIDI_RELAX_CONFIRM_WINDOWS {
                    self.last_reason = TidiTransitionReason::AwaitRelaxConfirmation
                        .as_str()
                        .to_string();
                } else if hold_elapsed {
                    announced = self.schedule_transition(
                        now_secs,
                        current_stamp,
                        TIDI_MAX_FACTOR,
                        TidiTransitionReason::Relax.as_str(),
                        TIDI_ADVANCE_NOTICE_SECS,
                    );
                    if announced.is_some() {
                        self.last_reason = TidiTransitionReason::Relax.as_str().to_string();
                    }
                } else {
                    self.last_reason = TidiTransitionReason::MinHoldActive.as_str().to_string();
                }
            } else {
                self.consecutive_low_load_windows = 0;
                self.last_reason = TidiTransitionReason::Normal.as_str().to_string();
            }
        } else {
            // Deadband `(15 ms ..= 20 ms]`: rising above relax threshold cancels pending relax and
            // clears low-load confirmation streak without engaging tighter dilation.
            self.consecutive_low_load_windows = 0;
            if self
                .pending_transition
                .as_ref()
                .is_some_and(|p| p.to_factor > self.current_factor + TIDI_MIN_CHANGE_DELTA)
            {
                self.pending_transition = None;
            }
            if self.pending_transition.is_none() {
                self.last_reason = TidiTransitionReason::Normal.as_str().to_string();
            }
        }

        // Edge case: if notice period was 0 (not in normal config, but guarded cleanly).
        if applied.is_none() {
            applied = self.poll_due_transition(now_secs, current_stamp);
        }

        self.push_decision_log(
            now_secs,
            safe_lateness,
            self.last_packet_perf_latency_ms,
            target_factor,
            self.last_reason.clone(),
        );

        AutoscalerWindowOutcome {
            announced,
            applied,
            reason: self.last_reason.clone(),
            target_factor,
        }
    }

    fn push_decision_log(
        &mut self,
        timestamp_secs: f64,
        lateness_ms: f64,
        packet_perf_latency_ms: Option<f64>,
        target_factor: f64,
        reason: String,
    ) {
        if self.decision_log.len() >= TIDI_MAX_DECISION_LOG_ENTRIES {
            self.decision_log.pop_front();
        }
        self.decision_log.push_back(TidiDecisionRecord {
            timestamp_secs,
            lateness_ms,
            packet_perf_latency_ms,
            current_factor: self.current_factor,
            target_factor,
            reason,
            pending: self.pending_transition.clone(),
        });
    }

    /// Exports an immutable observability snapshot for `/tidi status` and telemetry endpoints.
    pub fn snapshot(&self) -> SystemTidiSnapshot {
        SystemTidiSnapshot {
            solar_system_id: self.solar_system_id,
            enabled: self.enabled,
            pinned: self.pinned_factor.is_some(),
            factor: self.current_factor,
            lateness_ms: self.last_lateness_ms,
            packet_perf_latency_ms: self.last_packet_perf_latency_ms,
            reason: self.last_reason.clone(),
            pending_transition: self.pending_transition.clone(),
            consecutive_low_load_windows: self.consecutive_low_load_windows,
            last_transition_secs: self.last_transition_secs,
            decision_log: self.decision_log.iter().cloned().collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formula_matches_spec_and_clamps_bounds() {
        assert_eq!(compute_tidi_factor_from_lateness(0.0), 1.0);
        assert_eq!(compute_tidi_factor_from_lateness(20.0), 1.0);
        assert_eq!(compute_tidi_factor_from_lateness(f64::NAN), 1.0);
        let f_220 = compute_tidi_factor_from_lateness(220.0);
        assert!((f_220 - (1000.0 / 1200.0)).abs() < 1e-9);
        let f_1020 = compute_tidi_factor_from_lateness(1020.0);
        assert!((f_1020 - 0.50).abs() < 1e-9);
        assert_eq!(compute_tidi_factor_from_lateness(50_000.0), 0.10);
    }

    #[test]
    fn hysteresis_requires_two_low_load_windows_and_min_hold_and_clears_on_rising_load() {
        let mut s = SystemTidiAutoscaler::new(30_000_142, true);
        // Immediate tighten announcement on window 0 with 2s advance notice
        let out0 = s.observe_window(0.0, 100, 1020.0, Some(1020.0));
        assert_eq!(out0.reason, "tighten");
        let pend0 = out0.announced.expect("tighten must schedule advance notice");
        assert!((pend0.to_factor - 0.50).abs() < 1e-6);
        assert_eq!(pend0.target_stamp, 102);
        assert!((s.current_factor - 1.0).abs() < 1e-9, "must wait for advance notice");

        // Stamp 101 / t=1.0s: still pending
        let out1 = s.observe_window(1.0, 101, 1020.0, None);
        assert!(out1.applied.is_none());

        // Stamp 102 / t=2.0s: applies at boundary
        let out2 = s.observe_window(2.0, 102, 5.0, None);
        let app2 = out2.applied.expect("switch must apply at stamp 102 / t=2.0");
        assert!((app2.new_factor - 0.50).abs() < 1e-6);
        assert_eq!(out2.reason, "await-relax-confirmation");
        assert_eq!(s.consecutive_low_load_windows, 1);

        // Rising load before 2nd low-load window resets confirmation streak
        let out3 = s.observe_window(3.0, 103, 18.0, None);
        assert_eq!(s.consecutive_low_load_windows, 0);
        assert!(s.pending_transition.is_none());
        assert_eq!(out3.reason, "normal");

        // 1st low-load window at t=6.0 -> await-relax-confirmation
        let out6 = s.observe_window(6.0, 106, 5.0, None);
        assert_eq!(out6.reason, "await-relax-confirmation");
        assert!(out6.announced.is_none());

        // 2nd consecutive low-load window at t=7.0 (min hold 5s since applied at t=2.0 elapsed!) -> relax announced
        let out7 = s.observe_window(7.0, 107, 5.0, None);
        assert_eq!(out7.reason, "relax");
        let pend_relax = out7.announced.expect("2nd consecutive low-load window after hold must announce relax");
        assert!((pend_relax.to_factor - 1.0).abs() < 1e-6);
        assert_eq!(pend_relax.target_stamp, 109);

        // Rising load at t=8.0 cancels pending relax before t=9.0
        let _out8 = s.observe_window(8.0, 108, 25.0, None);
        assert!(s.pending_transition.is_none(), "rising load must clear pending relax");
        assert_eq!(s.consecutive_low_load_windows, 0);

        // Two consecutive low-load windows at t=9.0, 10.0 re-schedule relax, applying at t=12.0
        assert_eq!(s.observe_window(9.0, 109, 5.0, None).reason, "await-relax-confirmation");
        let r10 = s.observe_window(10.0, 110, 5.0, None);
        assert_eq!(r10.reason, "relax");
        assert!(r10.announced.is_some());
        let r12 = s.observe_window(12.0, 112, 5.0, None);
        let app_relax = r12.applied.expect("relax applies after 2s advance notice");
        assert!((app_relax.new_factor - 1.0).abs() < 1e-6);
        assert!((s.current_factor - 1.0).abs() < 1e-6);
    }

    #[test]
    fn clear_forces_one_and_skips_if_already_at_target() {
        let mut s = SystemTidiAutoscaler::new(30_000_142, true);
        assert!(s.schedule_transition(0.0, 10, 1.0, "relax", 2.0).is_none());
        s.pin_manual(1.0, 0.4);
        assert!((s.current_factor - 0.4).abs() < 1e-9);
        let (old, new) = s.clear_manual(2.0);
        assert!((old - 0.4).abs() < 1e-9);
        assert!((new - 1.0).abs() < 1e-9);
        assert!(s.pinned_factor.is_none());
    }
}
