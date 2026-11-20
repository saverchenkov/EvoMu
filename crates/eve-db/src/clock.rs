//! Injectable server clock and virtual time control (`T-7`).
//!
//! Centralizes all wall-clock queries in `eve-db` and `eve-server` behind the [`ServerClock`]
//! trait, supporting deterministic manual clock advancement (`advance_secs`), time-acceleration
//! multipliers (`acceleration_multiplier` / `set_acceleration_multiplier`), and pause-on-tick
//! (`pause_on_tick` / `set_pause_on_tick` / `set_paused`).

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// Default fallback Unix timestamp if the host system clock is prior to `UNIX_EPOCH`.
pub const DEFAULT_FALLBACK_UNIX_SECS: u64 = 1_760_000_000;

/// Difference between the Windows FILETIME epoch (`1601-01-01T00:00:00Z`) and
/// the Unix epoch (`1970-01-01T00:00:00Z`) in whole seconds.
pub const FILETIME_UNIX_EPOCH_DIFF_SECS: i64 = 11_644_473_600;

/// Difference between the Windows FILETIME epoch and the Unix epoch in milliseconds.
pub const FILETIME_UNIX_EPOCH_DIFF_MS: i64 = FILETIME_UNIX_EPOCH_DIFF_SECS * 1_000;

/// Number of 100-nanosecond Windows FILETIME ticks per millisecond.
pub const FILETIME_TICKS_PER_MS: i64 = 10_000;

/// Number of 100-nanosecond Windows FILETIME ticks per second.
pub const FILETIME_TICKS_PER_SEC: i64 = 10_000_000;

const FILETIME_UNIX_EPOCH_OFFSET_TICKS: i64 = FILETIME_UNIX_EPOCH_DIFF_MS * FILETIME_TICKS_PER_MS;

/// Number of dilated simulation milliseconds in one Destiny tick (`1 Hz`).
pub const DESTINY_TICK_MS: u64 = 1_000;

/// Minimum allowed Time Dilation factor (`0.10`, matching the system autoscaler floor).
pub const MIN_TIDI_FACTOR: f64 = 0.10;

/// Maximum allowed Time Dilation factor (`1.00`, real-time).
pub const MAX_TIDI_FACTOR: f64 = 1.00;

const TIDI_PPB_SCALE: u64 = 1_000_000_000;
const MIN_TIDI_PPB: u64 = 100_000_000;

/// Unix wall-clock milliseconds (`ServerClock` / real-time domain).
///
/// Distinct at compile time from [`SimDilatedMs`] so real-time durations and
/// dilated solar-system simulation timestamps cannot be mixed without an explicit
/// [`SolarSystemSimClock`] conversion.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, serde::Serialize, serde::Deserialize,
)]
pub struct WallUnixMs(pub u64);

impl WallUnixMs {
    /// Constructs a [`WallUnixMs`] from raw Unix milliseconds.
    #[must_use]
    pub const fn from_millis(ms: u64) -> Self {
        Self(ms)
    }

    /// Returns the raw Unix millisecond count.
    #[must_use]
    pub const fn as_millis(self) -> u64 {
        self.0
    }

    /// Converts whole Unix seconds into [`WallUnixMs`], returning `None` on overflow.
    #[must_use]
    pub const fn checked_from_secs(secs: u64) -> Option<Self> {
        match secs.checked_mul(1_000) {
            Some(ms) => Some(Self(ms)),
            None => None,
        }
    }

    /// Returns whole Unix seconds rounded down.
    #[must_use]
    pub const fn as_secs(self) -> u64 {
        self.0 / 1_000
    }

    /// Adds `delta_ms` using checked arithmetic.
    #[must_use]
    pub const fn checked_add_ms(self, delta_ms: u64) -> Option<Self> {
        match self.0.checked_add(delta_ms) {
            Some(ms) => Some(Self(ms)),
            None => None,
        }
    }

    /// Subtracts `delta_ms` using checked arithmetic.
    #[must_use]
    pub const fn checked_sub_ms(self, delta_ms: u64) -> Option<Self> {
        match self.0.checked_sub(delta_ms) {
            Some(ms) => Some(Self(ms)),
            None => None,
        }
    }

    /// Returns the elapsed wall milliseconds since `earlier`, or `None` if `self < earlier`.
    #[must_use]
    pub const fn checked_elapsed_since(self, earlier: Self) -> Option<u64> {
        self.0.checked_sub(earlier.0)
    }

    /// Returns the elapsed wall milliseconds since `earlier`, saturating at `0`.
    #[must_use]
    pub const fn saturating_elapsed_since(self, earlier: Self) -> u64 {
        self.0.saturating_sub(earlier.0)
    }

    /// Converts Unix wall-clock milliseconds into [`WallFileTime`] ticks, returning `None`
    /// if the result overflows `i64`.
    #[must_use]
    pub const fn to_wall_filetime(self) -> Option<WallFileTime> {
        if self.0 > i64::MAX as u64 {
            return None;
        }
        let Some(total_ms) = (self.0 as i64).checked_add(FILETIME_UNIX_EPOCH_DIFF_MS) else {
            return None;
        };
        match total_ms.checked_mul(FILETIME_TICKS_PER_MS) {
            Some(ticks) => Some(WallFileTime(ticks)),
            None => None,
        }
    }
}

/// Windows FILETIME ticks (100-nanosecond intervals since `1601-01-01`) in the wall-clock domain.
///
/// Distinct at compile time from [`SimFileTime`]; no arithmetic or implicit conversion may mix
/// wall-clock timestamps with TiDi-dilated simulation timestamps.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, serde::Serialize, serde::Deserialize,
)]
pub struct WallFileTime(pub i64);

impl WallFileTime {
    /// Constructs a [`WallFileTime`] from raw 100-nanosecond ticks.
    #[must_use]
    pub const fn from_ticks(ticks: i64) -> Self {
        Self(ticks)
    }

    /// Returns the raw 100-nanosecond FILETIME tick count.
    #[must_use]
    pub const fn as_ticks(self) -> i64 {
        self.0
    }

    /// Converts [`WallFileTime`] ticks back into [`WallUnixMs`], returning `None` if the
    /// timestamp predates `1970-01-01T00:00:00Z` or underflows `i64`.
    #[must_use]
    pub const fn to_wall_unix_ms(self) -> Option<WallUnixMs> {
        let Some(unix_ticks) = self.0.checked_sub(FILETIME_UNIX_EPOCH_OFFSET_TICKS) else {
            return None;
        };
        if unix_ticks < 0 {
            return None;
        }
        Some(WallUnixMs((unix_ticks / FILETIME_TICKS_PER_MS) as u64))
    }

    /// Adds `delta_ticks` using checked arithmetic.
    #[must_use]
    pub const fn checked_add_ticks(self, delta_ticks: i64) -> Option<Self> {
        match self.0.checked_add(delta_ticks) {
            Some(ticks) => Some(Self(ticks)),
            None => None,
        }
    }

    /// Subtracts `delta_ticks` using checked arithmetic.
    #[must_use]
    pub const fn checked_sub_ticks(self, delta_ticks: i64) -> Option<Self> {
        match self.0.checked_sub(delta_ticks) {
            Some(ticks) => Some(Self(ticks)),
            None => None,
        }
    }

    /// Returns the signed tick difference `self - earlier`, or `None` on overflow.
    #[must_use]
    pub const fn checked_diff_ticks(self, earlier: Self) -> Option<i64> {
        self.0.checked_sub(earlier.0)
    }
}

/// Monotonic TiDi-dilated simulation milliseconds for a solar system (`1970-01-01` epoch scale).
///
/// Distinct at compile time from [`WallUnixMs`].
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, serde::Serialize, serde::Deserialize,
)]
pub struct SimDilatedMs(pub u64);

impl SimDilatedMs {
    /// Constructs a [`SimDilatedMs`] from raw dilated milliseconds.
    #[must_use]
    pub const fn from_millis(ms: u64) -> Self {
        Self(ms)
    }

    /// Returns the raw dilated millisecond count.
    #[must_use]
    pub const fn as_millis(self) -> u64 {
        self.0
    }

    /// Returns whole dilated seconds rounded down.
    #[must_use]
    pub const fn as_secs(self) -> u64 {
        self.0 / 1_000
    }

    /// Adds `delta_ms` using checked arithmetic.
    #[must_use]
    pub const fn checked_add_ms(self, delta_ms: u64) -> Option<Self> {
        match self.0.checked_add(delta_ms) {
            Some(ms) => Some(Self(ms)),
            None => None,
        }
    }

    /// Subtracts `delta_ms` using checked arithmetic.
    #[must_use]
    pub const fn checked_sub_ms(self, delta_ms: u64) -> Option<Self> {
        match self.0.checked_sub(delta_ms) {
            Some(ms) => Some(Self(ms)),
            None => None,
        }
    }

    /// Returns elapsed dilated milliseconds since `earlier`, or `None` if `self < earlier`.
    #[must_use]
    pub const fn checked_elapsed_since(self, earlier: Self) -> Option<u64> {
        self.0.checked_sub(earlier.0)
    }

    /// Returns elapsed dilated milliseconds since `earlier`, saturating at `0`.
    #[must_use]
    pub const fn saturating_elapsed_since(self, earlier: Self) -> u64 {
        self.0.saturating_sub(earlier.0)
    }

    /// Converts dilated simulation milliseconds into [`SimFileTime`] ticks, returning `None`
    /// if the result overflows `i64`.
    #[must_use]
    pub const fn to_sim_filetime(self) -> Option<SimFileTime> {
        if self.0 > i64::MAX as u64 {
            return None;
        }
        let Some(total_ms) = (self.0 as i64).checked_add(FILETIME_UNIX_EPOCH_DIFF_MS) else {
            return None;
        };
        match total_ms.checked_mul(FILETIME_TICKS_PER_MS) {
            Some(ticks) => Some(SimFileTime(ticks)),
            None => None,
        }
    }

    /// Converts dilated simulation milliseconds into an epoch-derived [`DestinyStamp`], returning
    /// `None` if whole seconds exceed `u32::MAX`.
    #[must_use]
    pub const fn checked_to_destiny_stamp(self) -> Option<DestinyStamp> {
        let ticks = self.0 / DESTINY_TICK_MS;
        if ticks <= u32::MAX as u64 {
            Some(DestinyStamp(ticks as u32))
        } else {
            None
        }
    }

    /// Converts dilated simulation milliseconds into an epoch-derived [`DestinyStamp`], saturating
    /// at `u32::MAX`.
    #[must_use]
    pub const fn to_destiny_stamp_saturating(self) -> DestinyStamp {
        let ticks = self.0 / DESTINY_TICK_MS;
        if ticks > u32::MAX as u64 {
            DestinyStamp(u32::MAX)
        } else {
            DestinyStamp(ticks as u32)
        }
    }

    /// Returns elapsed dilated milliseconds within the current 1-second Destiny tick (`0..1000`).
    #[must_use]
    pub const fn subsec_ms(self) -> u64 {
        self.0 % DESTINY_TICK_MS
    }

    /// Returns remaining dilated milliseconds until the next 1-second Destiny tick boundary (`1..=1000`).
    #[must_use]
    pub const fn remaining_ms_in_tick(self) -> u64 {
        DESTINY_TICK_MS - (self.0 % DESTINY_TICK_MS)
    }
}

/// Windows FILETIME ticks (100-nanosecond intervals since `1601-01-01`) in the TiDi-dilated
/// simulation domain (`blue.os.GetSimTime()`).
///
/// Distinct at compile time from [`WallFileTime`]; client-visible module/session/timer comparisons
/// against simulation time must operate on `SimFileTime` rather than raw `WallFileTime`.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, serde::Serialize, serde::Deserialize,
)]
pub struct SimFileTime(pub i64);

impl SimFileTime {
    /// Constructs a [`SimFileTime`] from raw 100-nanosecond simulation ticks.
    #[must_use]
    pub const fn from_ticks(ticks: i64) -> Self {
        Self(ticks)
    }

    /// Returns the raw 100-nanosecond simulation FILETIME tick count.
    #[must_use]
    pub const fn as_ticks(self) -> i64 {
        self.0
    }

    /// Converts [`SimFileTime`] ticks back into [`SimDilatedMs`], returning `None` if the
    /// timestamp predates `1970-01-01T00:00:00Z` or underflows `i64`.
    #[must_use]
    pub const fn to_sim_dilated_ms(self) -> Option<SimDilatedMs> {
        let Some(unix_ticks) = self.0.checked_sub(FILETIME_UNIX_EPOCH_OFFSET_TICKS) else {
            return None;
        };
        if unix_ticks < 0 {
            return None;
        }
        Some(SimDilatedMs((unix_ticks / FILETIME_TICKS_PER_MS) as u64))
    }

    /// Adds `delta_ticks` using checked arithmetic.
    #[must_use]
    pub const fn checked_add_ticks(self, delta_ticks: i64) -> Option<Self> {
        match self.0.checked_add(delta_ticks) {
            Some(ticks) => Some(Self(ticks)),
            None => None,
        }
    }

    /// Subtracts `delta_ticks` using checked arithmetic.
    #[must_use]
    pub const fn checked_sub_ticks(self, delta_ticks: i64) -> Option<Self> {
        match self.0.checked_sub(delta_ticks) {
            Some(ticks) => Some(Self(ticks)),
            None => None,
        }
    }

    /// Returns the signed tick difference `self - earlier`, or `None` on overflow.
    #[must_use]
    pub const fn checked_diff_ticks(self, earlier: Self) -> Option<i64> {
        self.0.checked_sub(earlier.0)
    }
}

/// 1 Hz Destiny ballpark simulation stamp (`u32`).
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, serde::Serialize, serde::Deserialize,
)]
pub struct DestinyStamp(pub u32);

impl DestinyStamp {
    /// Constructs a [`DestinyStamp`] from raw 1 Hz tick count.
    #[must_use]
    pub const fn from_ticks(ticks: u32) -> Self {
        Self(ticks)
    }

    /// Returns the raw `u32` Destiny stamp.
    #[must_use]
    pub const fn as_ticks(self) -> u32 {
        self.0
    }

    /// Widens the stamp to `u64`.
    #[must_use]
    pub const fn as_u64(self) -> u64 {
        self.0 as u64
    }

    /// Advances the stamp by `delta` ticks using checked arithmetic.
    #[must_use]
    pub const fn checked_add_ticks(self, delta: u32) -> Option<Self> {
        match self.0.checked_add(delta) {
            Some(ticks) => Some(Self(ticks)),
            None => None,
        }
    }

    /// Returns elapsed Destiny ticks since `earlier`, saturating at `0`.
    #[must_use]
    pub const fn saturating_sub_ticks(self, earlier: Self) -> u32 {
        self.0.saturating_sub(earlier.0)
    }

    /// Converts an epoch-derived stamp into [`SimDilatedMs`], returning `None` on overflow.
    #[must_use]
    pub const fn checked_to_sim_dilated_ms(self) -> Option<SimDilatedMs> {
        match (self.0 as u64).checked_mul(DESTINY_TICK_MS) {
            Some(ms) => Some(SimDilatedMs(ms)),
            None => None,
        }
    }
}

/// Monotonic TiDi-aware solar-system simulation clock.
///
/// Tracks dilated simulation time (`SimDilatedMs` and monotonic `SimFileTime`), the current
/// TiDi factor (`0.10..=1.0`), `DestinyStamp`, and sub-tick remainder milliseconds across step
/// transitions without losing sub-millisecond or sub-tick progress and without backward jumps.
///
/// Fixed-point parts-per-billion scaling (`tidi_ppb`) eliminates IEEE-754 floor truncation
/// drift (such as `100.0 * 0.29 = 28.999999999999996`) while keeping `(sim_time - wallclock_time)`
/// offsets exact across TiDi step transitions.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SolarSystemSimClock {
    last_wall_ms: WallUnixMs,
    sim_dilated_ms: SimDilatedMs,
    sim_filetime: SimFileTime,
    tidi_factor: f64,
    tidi_ppb: u64,
    sub_ms_ppb: u64,
    destiny_stamp: DestinyStamp,
    stamp_remainder_ms: u64,
}

impl SolarSystemSimClock {
    /// Validates and clamps a finite TiDi factor into `[MIN_TIDI_FACTOR, MAX_TIDI_FACTOR]`.
    /// Returns `None` for non-finite inputs (`NaN`, `+/-inf`).
    #[must_use]
    pub fn clamp_tidi_factor(factor: f64) -> Option<f64> {
        if !factor.is_finite() {
            return None;
        }
        Some(factor.clamp(MIN_TIDI_FACTOR, MAX_TIDI_FACTOR))
    }

    fn factor_to_ppb(clamped_factor: f64) -> u64 {
        let scaled = (clamped_factor * TIDI_PPB_SCALE as f64)
            .round()
            .clamp(MIN_TIDI_PPB as f64, TIDI_PPB_SCALE as f64);
        scaled as u64
    }

    /// Creates a new [`SolarSystemSimClock`] anchored at `initial_wall_ms` with a signed
    /// initial `(sim_time - wallclock_time)` offset in milliseconds, an initial `DestinyStamp`,
    /// and a TiDi factor clamped to `[0.10, 1.00]`.
    ///
    /// Returns `None` if `tidi_factor` is non-finite, if `initial_wall_ms + initial_sim_offset_ms`
    /// underflows below `0` or overflows `u64`, or if the resulting [`SimDilatedMs`] cannot be
    /// represented as [`SimFileTime`].
    #[must_use]
    pub fn new(
        initial_wall_ms: WallUnixMs,
        initial_sim_offset_ms: i64,
        initial_stamp: DestinyStamp,
        tidi_factor: f64,
    ) -> Option<Self> {
        let clamped_factor = Self::clamp_tidi_factor(tidi_factor)?;
        let initial_sim_i128 =
            i128::from(initial_wall_ms.as_millis()).checked_add(i128::from(initial_sim_offset_ms))?;
        let initial_sim_u64 = u64::try_from(initial_sim_i128).ok()?;
        let sim_dilated_ms = SimDilatedMs::from_millis(initial_sim_u64);
        let sim_filetime = sim_dilated_ms.to_sim_filetime()?;
        Some(Self {
            last_wall_ms: initial_wall_ms,
            sim_dilated_ms,
            sim_filetime,
            tidi_factor: clamped_factor,
            tidi_ppb: Self::factor_to_ppb(clamped_factor),
            sub_ms_ppb: 0,
            destiny_stamp: initial_stamp,
            stamp_remainder_ms: 0,
        })
    }

    /// Sets an initial sub-tick dilated remainder (`0..DESTINY_TICK_MS`), returning `None` if
    /// `remainder_ms >= DESTINY_TICK_MS`.
    #[must_use]
    pub fn with_stamp_remainder_ms(mut self, remainder_ms: u64) -> Option<Self> {
        if remainder_ms >= DESTINY_TICK_MS {
            return None;
        }
        self.stamp_remainder_ms = remainder_ms;
        Some(self)
    }

    /// Returns the most recent wall-clock millisecond anchor processed by this clock.
    #[must_use]
    pub fn last_wall_ms(&self) -> WallUnixMs {
        self.last_wall_ms
    }

    /// Returns the current wall-clock anchor in [`WallFileTime`], if representable.
    #[must_use]
    pub fn last_wall_filetime(&self) -> Option<WallFileTime> {
        self.last_wall_ms.to_wall_filetime()
    }

    /// Returns the current monotonic dilated simulation time in milliseconds.
    #[must_use]
    pub fn sim_dilated_ms(&self) -> SimDilatedMs {
        self.sim_dilated_ms
    }

    /// Returns the current monotonic simulation [`SimFileTime`].
    #[must_use]
    pub fn sim_filetime(&self) -> SimFileTime {
        self.sim_filetime
    }

    /// Returns the active clamped TiDi factor (`0.10..=1.00`).
    #[must_use]
    pub fn tidi_factor(&self) -> f64 {
        self.tidi_factor
    }

    /// Returns the current 1 Hz [`DestinyStamp`].
    #[must_use]
    pub fn destiny_stamp(&self) -> DestinyStamp {
        self.destiny_stamp
    }

    /// Returns accumulated dilated milliseconds within the current Destiny tick (`0..1000`).
    #[must_use]
    pub fn stamp_remainder_ms(&self) -> u64 {
        self.stamp_remainder_ms
    }

    /// Returns remaining dilated milliseconds until the next [`DestinyStamp`] increment (`1..=1000`).
    #[must_use]
    pub fn remaining_ms_in_tick(&self) -> u64 {
        DESTINY_TICK_MS - self.stamp_remainder_ms
    }

    /// Returns the current `(sim_time - wallclock_time)` offset in milliseconds, or `None` if
    /// the difference overflows `i64`.
    #[must_use]
    pub fn sim_minus_wall_offset_ms(&self) -> Option<i64> {
        let diff =
            i128::from(self.sim_dilated_ms.as_millis()) - i128::from(self.last_wall_ms.as_millis());
        i64::try_from(diff).ok()
    }

    /// Read-only projection of `wall_ms` (`>= self.last_wall_ms()`) into dilated simulation ms
    /// at the active TiDi factor, without mutating clock state. Returns `None` if `wall_ms`
    /// precedes `self.last_wall_ms()` or overflows `u64`.
    #[must_use]
    pub fn project_wall_to_sim_dilated_ms(&self, wall_ms: WallUnixMs) -> Option<SimDilatedMs> {
        let wall_delta_ms = wall_ms.checked_elapsed_since(self.last_wall_ms)?;
        let total_ppb = u128::from(wall_delta_ms)
            .checked_mul(u128::from(self.tidi_ppb))?
            .checked_add(u128::from(self.sub_ms_ppb))?;
        let sim_delta_ms = u64::try_from(total_ppb / u128::from(TIDI_PPB_SCALE)).ok()?;
        self.sim_dilated_ms.checked_add_ms(sim_delta_ms)
    }

    /// Read-only projection of `wall_ms` (`>= self.last_wall_ms()`) into [`SimFileTime`],
    /// preserving the monotonic floor `>= self.sim_filetime()`.
    #[must_use]
    pub fn project_wall_to_sim_filetime(&self, wall_ms: WallUnixMs) -> Option<SimFileTime> {
        let sim_ms = self.project_wall_to_sim_dilated_ms(wall_ms)?;
        let projected = sim_ms.to_sim_filetime()?;
        if projected < self.sim_filetime {
            Some(self.sim_filetime)
        } else {
            Some(projected)
        }
    }

    /// Returns how many dilated simulation milliseconds elapse over `wall_duration_ms` at the
    /// current TiDi factor (floored), returning `None` on `u64` overflow.
    #[must_use]
    pub fn sim_ms_for_wall_ms(&self, wall_duration_ms: u64) -> Option<u64> {
        let scaled = u128::from(wall_duration_ms).checked_mul(u128::from(self.tidi_ppb))?;
        u64::try_from(scaled / u128::from(TIDI_PPB_SCALE)).ok()
    }

    /// Returns the ceiling wall-clock milliseconds required for `sim_duration_ms` of dilated
    /// simulation time to elapse at the current TiDi factor, returning `None` on overflow.
    #[must_use]
    pub fn wall_ms_needed_for_sim_ms(&self, sim_duration_ms: u64) -> Option<u64> {
        let denom = u128::from(self.tidi_ppb);
        let scaled = u128::from(sim_duration_ms).checked_mul(u128::from(TIDI_PPB_SCALE))?;
        let rounded_up = scaled.checked_add(denom - 1)? / denom;
        u64::try_from(rounded_up).ok()
    }

    /// Advances the simulation clock monotonically by `wall_delta_ms` using the active TiDi
    /// factor, carrying sub-millisecond and sub-tick progress cleanly.
    ///
    /// Performs all checked arithmetic before mutating `self`, leaving state untouched and
    /// returning `None` if any timestamp or stamp overflows.
    #[must_use]
    pub fn advance_wall_ms(&mut self, wall_delta_ms: u64) -> Option<SimDilatedMs> {
        let scale = u128::from(TIDI_PPB_SCALE);
        let total_ppb = u128::from(wall_delta_ms)
            .checked_mul(u128::from(self.tidi_ppb))?
            .checked_add(u128::from(self.sub_ms_ppb))?;
        let sim_delta_ms = u64::try_from(total_ppb / scale).ok()?;
        let new_sub_ms_ppb = (total_ppb % scale) as u64;

        let new_wall_ms = self.last_wall_ms.checked_add_ms(wall_delta_ms)?;
        let new_sim_ms = self.sim_dilated_ms.checked_add_ms(sim_delta_ms)?;
        let new_sim_ft = new_sim_ms.to_sim_filetime()?;
        if new_sim_ft < self.sim_filetime {
            return None;
        }

        let tick_ms = u128::from(DESTINY_TICK_MS);
        let total_stamp_ms =
            u128::from(self.stamp_remainder_ms).checked_add(u128::from(sim_delta_ms))?;
        let stamp_ticks_delta = u32::try_from(total_stamp_ms / tick_ms).ok()?;
        let new_stamp = self.destiny_stamp.checked_add_ticks(stamp_ticks_delta)?;
        let new_stamp_remainder_ms = (total_stamp_ms % tick_ms) as u64;

        self.last_wall_ms = new_wall_ms;
        self.sim_dilated_ms = new_sim_ms;
        self.sim_filetime = new_sim_ft;
        self.sub_ms_ppb = new_sub_ms_ppb;
        self.destiny_stamp = new_stamp;
        self.stamp_remainder_ms = new_stamp_remainder_ms;
        Some(self.sim_dilated_ms)
    }

    /// Advances the simulation clock monotonically to `wall_now_ms`.
    ///
    /// If `wall_now_ms <= self.last_wall_ms()`, no backward jump occurs and the current
    /// [`SimDilatedMs`] is returned unchanged.
    #[must_use]
    pub fn advance_to_wall_ms(&mut self, wall_now_ms: WallUnixMs) -> Option<SimDilatedMs> {
        if wall_now_ms <= self.last_wall_ms {
            return Some(self.sim_dilated_ms);
        }
        let delta = wall_now_ms.saturating_elapsed_since(self.last_wall_ms);
        self.advance_wall_ms(delta)
    }

    /// Updates the TiDi factor at the current anchor without advancing wall time or jumping
    /// [`SimFileTime`] backward. Returns `None` if `new_factor` is non-finite.
    #[must_use]
    pub fn set_tidi_factor(&mut self, new_factor: f64) -> Option<f64> {
        let clamped = Self::clamp_tidi_factor(new_factor)?;
        self.tidi_factor = clamped;
        self.tidi_ppb = Self::factor_to_ppb(clamped);
        Some(clamped)
    }

    /// Advances monotonically to `wall_now_ms` under the outgoing TiDi factor, then switches to
    /// the new clamped TiDi factor (`[0.10, 1.00]`).
    ///
    /// Validates `new_factor` first so an invalid factor leaves `self` completely unchanged.
    #[must_use]
    pub fn set_tidi_factor_at(&mut self, wall_now_ms: WallUnixMs, new_factor: f64) -> Option<f64> {
        let clamped = Self::clamp_tidi_factor(new_factor)?;
        self.advance_to_wall_ms(wall_now_ms)?;
        self.tidi_factor = clamped;
        self.tidi_ppb = Self::factor_to_ppb(clamped);
        Some(clamped)
    }
}

/// Reads the host OS wall clock in Unix seconds (`SystemTime::now()`).
pub fn host_wall_unix_secs_and_nanos() -> (u64, u32) {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| (d.as_secs(), d.subsec_nanos()))
        .unwrap_or((DEFAULT_FALLBACK_UNIX_SECS, 0))
}

/// Injectable server clock trait (`T-7`) used by `WorldRepository` and `ServerContext`.
pub trait ServerClock: Send + Sync {
    /// Current server Unix timestamp in seconds.
    fn now_unix_secs(&self) -> u64;

    /// Current sub-second nanoseconds (`0..1_000_000_000`).
    fn now_subsec_nanos(&self) -> u32 {
        0
    }

    /// Current Windows FILETIME / BlueOS timestamp in 100-ns ticks since `1601-01-01`.
    fn now_blueos_filetime_ticks(&self) -> i64 {
        let secs = self.now_unix_secs() as i64;
        let nanos = self.now_subsec_nanos() as i64;
        (secs + 11_644_473_600) * 10_000_000 + (nanos / 100)
    }

    /// Advances virtual time by `secs` seconds immediately.
    fn advance_secs(&self, secs: u64);

    /// Advances virtual time by `millis` milliseconds immediately.
    fn advance_millis(&self, millis: u64) {
        if millis >= 1_000 {
            self.advance_secs(millis / 1_000);
        }
    }

    /// Current virtual timestamp in Unix milliseconds.
    fn now_unix_millis(&self) -> u64 {
        self.now_unix_secs()
            .saturating_mul(1_000)
            .saturating_add((self.now_subsec_nanos() / 1_000_000) as u64)
    }

    /// Deterministic test override pinning virtual wall time to `unix_secs`.
    #[cfg(any(test, feature = "testkit"))]
    fn set_wall_now_for_tests(&self, _unix_secs: u64) {}

    /// Deterministic test override pinning virtual wall time to `unix_millis`.
    #[cfg(any(test, feature = "testkit"))]
    fn set_wall_now_ms_for_tests(&self, _unix_millis: u64) {}

    /// Returns the cumulative virtual time offset in seconds (`0` for un-offset wall clocks).
    fn offset_secs(&self) -> u64 {
        0
    }

    /// Restores a previously persisted `world_meta.clock_offset_secs` offset on startup.
    fn restore_offset_secs(&self, _offset_secs: u64) {}

    /// Returns the configured time-acceleration multiplier (`>= 1`).
    fn acceleration_multiplier(&self) -> u64;

    /// Sets the time-acceleration multiplier (`>= 1`).
    fn set_acceleration_multiplier(&self, multiplier: u64);

    /// Returns `true` if the clock is currently paused.
    fn is_paused(&self) -> bool;

    /// Pauses or unpauses the clock. While paused, wall-clock drift is frozen.
    fn set_paused(&self, paused: bool);

    /// Returns `true` if pause-on-tick mode is enabled.
    fn pause_on_tick(&self) -> bool;

    /// Enables or disables automatic pause after each simulation tick.
    fn set_pause_on_tick(&self, enabled: bool);

    /// Advances the clock by one simulation tick of `base_secs` real seconds multiplied by
    /// [`Self::acceleration_multiplier`]. Returns the number of virtual seconds advanced (`0` if paused).
    /// When [`Self::pause_on_tick`] is enabled, automatically pauses the clock after advancing.
    fn step_tick(&self, base_secs: u64) -> u64;

    /// Advances a live 1 Hz wall-clock loop by one real second (`T-7`), adding `(multiplier - 1)`
    /// extra offset seconds on a wall-anchored clock so `1x` does not double-count `SystemTime::now()`.
    fn tick_wall_second(&self) -> u64 {
        self.step_tick(1)
    }
}

/// Default thread-safe [`ServerClock`] implementation supporting virtual time offset,
/// fixed/frozen base timestamps, time-acceleration multipliers, and pause-on-tick (`T-7`).
#[derive(Debug)]
pub struct VirtualServerClock {
    /// Non-zero when created via [`VirtualServerClock::with_fixed_base`]; `0` when wall-clock-anchored.
    fixed_base_secs: AtomicU64,
    /// Host wall-clock timestamp captured when a wall-anchored clock enters paused state (`0` when running).
    paused_at_wall_secs: AtomicU64,
    /// Cumulative wall-clock seconds spent paused so unpausing resumes monotonically without jumping.
    paused_accum_wall_secs: AtomicU64,
    /// Cumulative virtual clock offset in milliseconds.
    offset_millis: AtomicU64,
    /// Time-acceleration factor applied by [`Self::step_tick`] (`>= 1`).
    acceleration_multiplier: AtomicU64,
    /// Whether the clock is currently paused.
    paused: AtomicBool,
    /// Whether the clock should automatically pause at the end of [`Self::step_tick`].
    pause_on_tick: AtomicBool,
}

impl Default for VirtualServerClock {
    fn default() -> Self {
        Self::new()
    }
}

impl VirtualServerClock {
    fn from_parts(fixed_base_secs: u64, offset_secs: u64) -> Self {
        Self {
            fixed_base_secs: AtomicU64::new(fixed_base_secs),
            paused_at_wall_secs: AtomicU64::new(0),
            paused_accum_wall_secs: AtomicU64::new(0),
            offset_millis: AtomicU64::new(offset_secs.saturating_mul(1_000)),
            acceleration_multiplier: AtomicU64::new(1),
            paused: AtomicBool::new(false),
            pause_on_tick: AtomicBool::new(false),
        }
    }

    /// Creates a new `VirtualServerClock` anchored to the host wall clock with `1x` multiplier.
    pub fn new() -> Self {
        Self::from_parts(0, 0)
    }

    /// Creates a wall-clock-anchored `VirtualServerClock` initialized with a persisted `offset_secs`.
    pub fn with_offset_secs(offset_secs: u64) -> Self {
        Self::from_parts(0, offset_secs)
    }

    /// Creates a deterministic `VirtualServerClock` anchored at a fixed `initial_unix_secs` timestamp.
    pub fn with_fixed_base(initial_unix_secs: u64) -> Self {
        Self::from_parts(initial_unix_secs.max(1), 0)
    }

    /// Creates a deterministic `VirtualServerClock` anchored at `initial_unix_secs` with `offset_secs`.
    pub fn with_fixed_base_and_offset(initial_unix_secs: u64, offset_secs: u64) -> Self {
        Self::from_parts(initial_unix_secs.max(1), offset_secs)
    }

    /// Returns the cumulative virtual time offset in whole seconds.
    pub fn offset_secs(&self) -> u64 {
        self.offset_millis.load(Ordering::SeqCst) / 1_000
    }

    /// Restores the persisted virtual clock offset in seconds.
    pub fn restore_offset_secs(&self, offset_secs: u64) {
        self.offset_millis
            .store(offset_secs.saturating_mul(1_000), Ordering::SeqCst);
    }

    /// Atomically advances virtual time by `secs` seconds and returns the updated cumulative offset.
    pub fn advance_secs_and_read_offset(&self, secs: u64) -> u64 {
        let delta_ms = secs.saturating_mul(1_000);
        let prev = self.offset_millis.fetch_add(delta_ms, Ordering::SeqCst);
        prev.saturating_add(delta_ms) / 1_000
    }

    fn base_secs_and_nanos(&self) -> (u64, u32) {
        let fixed = self.fixed_base_secs.load(Ordering::SeqCst);
        if fixed > 0 {
            return (fixed, 0);
        }
        let accum = self.paused_accum_wall_secs.load(Ordering::SeqCst);
        if self.paused.load(Ordering::SeqCst) {
            let paused_at = self.paused_at_wall_secs.load(Ordering::SeqCst);
            let anchor = if paused_at > 0 {
                paused_at
            } else {
                host_wall_unix_secs_and_nanos().0
            };
            (anchor.saturating_sub(accum), 0)
        } else {
            let (wall_secs, wall_nanos) = host_wall_unix_secs_and_nanos();
            (wall_secs.saturating_sub(accum), wall_nanos)
        }
    }

    fn current_secs_and_nanos(&self) -> (u64, u32) {
        let (base_secs, base_nanos) = self.base_secs_and_nanos();
        let offset_ms = self.offset_millis.load(Ordering::SeqCst);
        let extra_nanos = ((offset_ms % 1_000) as u32) * 1_000_000;
        let total_nanos = base_nanos + extra_nanos;
        let carry_secs = (total_nanos / 1_000_000_000) as u64;
        let secs = base_secs
            .saturating_add(offset_ms / 1_000)
            .saturating_add(carry_secs);
        (secs, total_nanos % 1_000_000_000)
    }

    /// Deterministically pins virtual wall time to `unix_secs` (resetting virtual offset to `0`).
    #[cfg(any(test, feature = "testkit"))]
    pub fn set_wall_now_for_tests(&self, unix_secs: u64) {
        self.fixed_base_secs
            .store(unix_secs.max(1), Ordering::SeqCst);
        self.offset_millis.store(0, Ordering::SeqCst);
        self.paused_at_wall_secs.store(0, Ordering::SeqCst);
        self.paused_accum_wall_secs.store(0, Ordering::SeqCst);
    }

    /// Deterministically pins virtual wall time to `unix_millis`.
    #[cfg(any(test, feature = "testkit"))]
    pub fn set_wall_now_ms_for_tests(&self, unix_millis: u64) {
        let secs = (unix_millis / 1_000).max(1);
        let rem_ms = if unix_millis >= 1_000 {
            unix_millis % 1_000
        } else {
            0
        };
        self.fixed_base_secs.store(secs, Ordering::SeqCst);
        self.offset_millis.store(rem_ms, Ordering::SeqCst);
        self.paused_at_wall_secs.store(0, Ordering::SeqCst);
        self.paused_accum_wall_secs.store(0, Ordering::SeqCst);
    }
}

impl ServerClock for VirtualServerClock {
    fn now_unix_secs(&self) -> u64 {
        self.current_secs_and_nanos().0
    }

    fn now_subsec_nanos(&self) -> u32 {
        self.current_secs_and_nanos().1
    }

    fn advance_secs(&self, secs: u64) {
        self.offset_millis
            .fetch_add(secs.saturating_mul(1_000), Ordering::SeqCst);
    }

    fn advance_millis(&self, millis: u64) {
        self.offset_millis.fetch_add(millis, Ordering::SeqCst);
    }

    #[cfg(any(test, feature = "testkit"))]
    fn set_wall_now_for_tests(&self, unix_secs: u64) {
        VirtualServerClock::set_wall_now_for_tests(self, unix_secs);
    }

    #[cfg(any(test, feature = "testkit"))]
    fn set_wall_now_ms_for_tests(&self, unix_millis: u64) {
        VirtualServerClock::set_wall_now_ms_for_tests(self, unix_millis);
    }

    fn offset_secs(&self) -> u64 {
        VirtualServerClock::offset_secs(self)
    }

    fn restore_offset_secs(&self, offset_secs: u64) {
        VirtualServerClock::restore_offset_secs(self, offset_secs);
    }

    fn acceleration_multiplier(&self) -> u64 {
        self.acceleration_multiplier.load(Ordering::SeqCst).max(1)
    }

    fn set_acceleration_multiplier(&self, multiplier: u64) {
        self.acceleration_multiplier
            .store(multiplier.max(1), Ordering::SeqCst);
    }

    fn is_paused(&self) -> bool {
        self.paused.load(Ordering::SeqCst)
    }

    fn set_paused(&self, paused: bool) {
        let was_paused = self.paused.swap(paused, Ordering::SeqCst);
        if self.fixed_base_secs.load(Ordering::SeqCst) == 0 {
            let now_wall = host_wall_unix_secs_and_nanos().0.max(1);
            if paused && !was_paused {
                self.paused_at_wall_secs.store(now_wall, Ordering::SeqCst);
            } else if !paused && was_paused {
                let paused_at = self.paused_at_wall_secs.swap(0, Ordering::SeqCst);
                if paused_at > 0 && now_wall > paused_at {
                    self.paused_accum_wall_secs
                        .fetch_add(now_wall - paused_at, Ordering::SeqCst);
                }
            }
        }
    }

    fn pause_on_tick(&self) -> bool {
        self.pause_on_tick.load(Ordering::SeqCst)
    }

    fn set_pause_on_tick(&self, enabled: bool) {
        self.pause_on_tick.store(enabled, Ordering::SeqCst);
    }

    fn step_tick(&self, base_secs: u64) -> u64 {
        if self.is_paused() {
            return 0;
        }
        let mult = self.acceleration_multiplier();
        let advanced = base_secs.saturating_mul(mult);
        self.offset_millis
            .fetch_add(advanced.saturating_mul(1_000), Ordering::SeqCst);
        if self.pause_on_tick() {
            self.set_paused(true);
        }
        advanced
    }

    fn tick_wall_second(&self) -> u64 {
        if self.is_paused() {
            return 0;
        }
        if self.fixed_base_secs.load(Ordering::SeqCst) > 0 {
            return self.step_tick(1);
        }
        let mult = self.acceleration_multiplier();
        if mult > 1 {
            self.offset_millis
                .fetch_add((mult - 1).saturating_mul(1_000), Ordering::SeqCst);
        }
        if self.pause_on_tick() {
            self.set_paused(true);
        }
        mult
    }
}

impl crate::WorldRepository {
    /// Reloads the persisted [`crate::WorldStateSnapshot`] directly from the attached SQLite
    /// connection without mutating in-memory state (`P2-0g`, `#908`).
    pub fn reload_snapshot_from_sqlite(&self) -> Result<crate::WorldStateSnapshot, String> {
        let writer = self.writer.lock().unwrap_or_else(|e| e.into_inner());
        crate::sqlite_store::load_snapshot_from_sqlite(&writer.conn)
    }

    /// Mutates the in-memory [`crate::WorldStateSnapshot`] without syncing to the attached SQLite
    /// connection (`#908`). Used by `eve-testkit` PRE check tests to verify that unpersisted
    /// in-memory mutations are detected on teardown.
    pub fn inject_unpersisted_mutation_for_test<F>(&self, f: F)
    where
        F: FnOnce(&mut crate::WorldStateSnapshot),
    {
        let mut writer = self.writer.lock().unwrap_or_else(|e| e.into_inner());
        f(&mut writer.working);
        *self.published.write().unwrap_or_else(|e| e.into_inner()) =
            std::sync::Arc::new(writer.working.clone());
    }
}

impl crate::WorldStateSnapshot {
    /// Reads the snapshot-mirrored virtual clock seconds (`__server_now_unix`), if present
    /// and valid, avoiding host `SystemTime::now()` queries in snapshot-only helpers.
    #[must_use]
    pub fn checked_snapshot_now_unix_secs(&self) -> Option<u64> {
        let raw = self.service_state_blobs.get("__server_now_unix")?.trim();
        if let Ok(secs) = raw.parse::<u64>() {
            return Some(secs);
        }
        let float_secs = raw.parse::<f64>().ok()?;
        if float_secs.is_finite() && float_secs >= 0.0 && float_secs <= u64::MAX as f64 {
            Some(float_secs.floor() as u64)
        } else {
            None
        }
    }

    /// Reads the snapshot-mirrored virtual clock seconds (`__server_now_unix`), defaulting to
    /// `0` when absent or unparseable (matching `EntityKvWriter::default_now_unix`).
    #[must_use]
    pub fn snapshot_now_unix_secs(&self) -> u64 {
        self.checked_snapshot_now_unix_secs().unwrap_or(0)
    }
}

#[cfg(test)]
#[path = "clock_unit_tests.rs"]
mod clock_unit_tests;

