//! Injectable server clock and virtual time control (`T-7`).
//!
//! Centralizes all wall-clock queries in `eve-db` and `eve-server` behind the [`ServerClock`]
//! trait, supporting deterministic manual clock advancement (`advance_secs`), time-acceleration
//! multipliers (`acceleration_multiplier` / `set_acceleration_multiplier`), and pause-on-tick
//! (`pause_on_tick` / `set_pause_on_tick` / `set_paused`).

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// Default fallback Unix timestamp if the host system clock is prior to `UNIX_EPOCH`.
pub const DEFAULT_FALLBACK_UNIX_SECS: u64 = 1_760_000_000;

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
    /// Cumulative virtual clock offset in seconds.
    offset_secs: AtomicU64,
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
    /// Creates a new `VirtualServerClock` anchored to the host wall clock with `1x` multiplier.
    pub fn new() -> Self {
        Self {
            fixed_base_secs: AtomicU64::new(0),
            paused_at_wall_secs: AtomicU64::new(0),
            paused_accum_wall_secs: AtomicU64::new(0),
            offset_secs: AtomicU64::new(0),
            acceleration_multiplier: AtomicU64::new(1),
            paused: AtomicBool::new(false),
            pause_on_tick: AtomicBool::new(false),
        }
    }

    /// Creates a deterministic `VirtualServerClock` anchored at a fixed `initial_unix_secs` timestamp.
    pub fn with_fixed_base(initial_unix_secs: u64) -> Self {
        Self {
            fixed_base_secs: AtomicU64::new(initial_unix_secs.max(1)),
            paused_at_wall_secs: AtomicU64::new(0),
            paused_accum_wall_secs: AtomicU64::new(0),
            offset_secs: AtomicU64::new(0),
            acceleration_multiplier: AtomicU64::new(1),
            paused: AtomicBool::new(false),
            pause_on_tick: AtomicBool::new(false),
        }
    }
}

impl ServerClock for VirtualServerClock {
    fn now_unix_secs(&self) -> u64 {
        let fixed = self.fixed_base_secs.load(Ordering::SeqCst);
        let base = if fixed > 0 {
            fixed
        } else {
            let accum = self.paused_accum_wall_secs.load(Ordering::SeqCst);
            if self.paused.load(Ordering::SeqCst) {
                let paused_at = self.paused_at_wall_secs.load(Ordering::SeqCst);
                let anchor = if paused_at > 0 {
                    paused_at
                } else {
                    host_wall_unix_secs_and_nanos().0
                };
                anchor.saturating_sub(accum)
            } else {
                host_wall_unix_secs_and_nanos().0.saturating_sub(accum)
            }
        };
        base.saturating_add(self.offset_secs.load(Ordering::SeqCst))
    }

    fn now_subsec_nanos(&self) -> u32 {
        if self.fixed_base_secs.load(Ordering::SeqCst) > 0 || self.paused.load(Ordering::SeqCst) {
            0
        } else {
            host_wall_unix_secs_and_nanos().1
        }
    }

    fn advance_secs(&self, secs: u64) {
        self.offset_secs.fetch_add(secs, Ordering::SeqCst);
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
        self.offset_secs.fetch_add(advanced, Ordering::SeqCst);
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
            self.offset_secs.fetch_add(mult - 1, Ordering::SeqCst);
        }
        if self.pause_on_tick() {
            self.set_paused(true);
        }
        mult
    }
}
