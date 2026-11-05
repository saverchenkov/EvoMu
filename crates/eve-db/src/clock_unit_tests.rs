//! Unit and mutation-killing tests for `clock.rs` (`ServerClock`, `VirtualServerClock`).

use super::*;
use std::sync::atomic::Ordering;

struct DummyTraitClock {
    secs: u64,
    nanos: u32,
    stepped: AtomicU64,
}

impl ServerClock for DummyTraitClock {
    fn now_unix_secs(&self) -> u64 {
        self.secs
    }

    fn now_subsec_nanos(&self) -> u32 {
        self.nanos
    }

    fn advance_secs(&self, secs: u64) {
        self.stepped.fetch_add(secs, Ordering::SeqCst);
    }

    fn acceleration_multiplier(&self) -> u64 {
        1
    }

    fn set_acceleration_multiplier(&self, _multiplier: u64) {}

    fn is_paused(&self) -> bool {
        false
    }

    fn set_paused(&self, _paused: bool) {}

    fn pause_on_tick(&self) -> bool {
        false
    }

    fn set_pause_on_tick(&self, _enabled: bool) {}

    fn step_tick(&self, base_secs: u64) -> u64 {
        self.stepped.fetch_add(base_secs, Ordering::SeqCst);
        base_secs
    }
}

struct DefaultNanosClock(u64);

impl ServerClock for DefaultNanosClock {
    fn now_unix_secs(&self) -> u64 {
        self.0
    }
    fn advance_secs(&self, _secs: u64) {}
    fn acceleration_multiplier(&self) -> u64 {
        1
    }
    fn set_acceleration_multiplier(&self, _multiplier: u64) {}
    fn is_paused(&self) -> bool {
        false
    }
    fn set_paused(&self, _paused: bool) {}
    fn pause_on_tick(&self) -> bool {
        false
    }
    fn set_pause_on_tick(&self, _enabled: bool) {}
    fn step_tick(&self, base_secs: u64) -> u64 {
        base_secs
    }
}

#[test]
fn host_wall_clock_and_default_trait_methods_work() {
    let (secs, nanos) = host_wall_unix_secs_and_nanos();
    assert!(secs >= DEFAULT_FALLBACK_UNIX_SECS || secs > 1_700_000_000);
    assert!(nanos < 1_000_000_000);

    let dummy = DummyTraitClock {
        secs: 1_700_000_000,
        nanos: 456_789_123,
        stepped: AtomicU64::new(0),
    };
    let expected_ft = (1_700_000_000_i64 + 11_644_473_600) * 10_000_000 + (456_789_123 / 100);
    assert_eq!(dummy.now_blueos_filetime_ticks(), expected_ft);
    assert_eq!(dummy.tick_wall_second(), 1);
    assert_eq!(dummy.stepped.load(Ordering::SeqCst), 1);

    let def_clock = DefaultNanosClock(1_700_000_005);
    assert_eq!(def_clock.now_subsec_nanos(), 0);
    assert_eq!(
        def_clock.now_blueos_filetime_ticks(),
        (1_700_000_005_i64 + 11_644_473_600) * 10_000_000
    );
}

#[test]
fn fixed_base_clock_clamps_zero_and_advances_deterministically() {
    let c0 = VirtualServerClock::with_fixed_base(0);
    assert_eq!(c0.now_unix_secs(), 1);
    assert_eq!(c0.now_subsec_nanos(), 0);

    let c = VirtualServerClock::with_fixed_base(1_750_000_000);
    assert_eq!(c.now_unix_secs(), 1_750_000_000);
    assert_eq!(c.now_subsec_nanos(), 0);
    assert_eq!(
        c.now_blueos_filetime_ticks(),
        (1_750_000_000_i64 + 11_644_473_600) * 10_000_000
    );

    c.advance_secs(42);
    assert_eq!(c.now_unix_secs(), 1_750_000_042);

    // Multiplier clamping at >= 1
    assert_eq!(c.acceleration_multiplier(), 1);
    c.set_acceleration_multiplier(0);
    assert_eq!(c.acceleration_multiplier(), 1);
    c.set_acceleration_multiplier(10);
    assert_eq!(c.acceleration_multiplier(), 10);

    // step_tick multiplies base_secs by acceleration_multiplier
    let adv = c.step_tick(3);
    assert_eq!(adv, 30);
    assert_eq!(c.now_unix_secs(), 1_750_000_072);

    // tick_wall_second on fixed-base delegates to step_tick(1)
    let adv_wall = c.tick_wall_second();
    assert_eq!(adv_wall, 10);
    assert_eq!(c.now_unix_secs(), 1_750_000_082);
}

#[test]
fn pause_and_pause_on_tick_behavior_on_fixed_base_clock() {
    let c = VirtualServerClock::with_fixed_base(1_750_000_000);
    assert!(!c.is_paused());
    assert!(!c.pause_on_tick());

    c.set_acceleration_multiplier(5);
    c.set_pause_on_tick(true);
    assert!(c.pause_on_tick());

    // First step_tick advances 2 * 5 = 10s and then auto-pauses
    assert_eq!(c.step_tick(2), 10);
    assert_eq!(c.now_unix_secs(), 1_750_000_010);
    assert!(c.is_paused());

    // While paused, step_tick and tick_wall_second return 0 and do not advance
    assert_eq!(c.step_tick(2), 0);
    assert_eq!(c.tick_wall_second(), 0);
    assert_eq!(c.now_unix_secs(), 1_750_000_010);

    // Unpause and disable pause_on_tick
    c.set_pause_on_tick(false);
    c.set_paused(false);
    assert!(!c.is_paused());
    assert_eq!(c.step_tick(2), 10);
    assert!(!c.is_paused());
    assert_eq!(c.now_unix_secs(), 1_750_000_020);
}

#[test]
fn wall_anchored_clock_pause_accumulation_and_tick_wall_second() {
    let c = VirtualServerClock::default();
    assert!(!c.is_paused());
    let t0 = c.now_unix_secs();
    assert!(t0 > 1_700_000_000);

    // 1x tick_wall_second on wall-anchored clock does not add extra offset
    assert_eq!(c.tick_wall_second(), 1);
    assert_eq!(c.offset_secs.load(Ordering::SeqCst), 0);

    // 4x tick_wall_second on wall-anchored clock adds (4 - 1) = 3 offset seconds
    c.set_acceleration_multiplier(4);
    assert_eq!(c.tick_wall_second(), 4);
    assert_eq!(c.offset_secs.load(Ordering::SeqCst), 3);

    // pause_on_tick on wall-anchored tick_wall_second
    c.set_pause_on_tick(true);
    assert_eq!(c.tick_wall_second(), 4);
    assert!(c.is_paused());
    assert_eq!(c.now_subsec_nanos(), 0);
    assert_eq!(c.tick_wall_second(), 0);

    // Simulate 25 seconds of wall-clock time spent paused by backdating paused_at_wall_secs
    let paused_wall = c.paused_at_wall_secs.load(Ordering::SeqCst);
    assert!(paused_wall > 0);
    c.paused_at_wall_secs
        .store(paused_wall.saturating_sub(25), Ordering::SeqCst);
    let frozen_reading = c.now_unix_secs();
    assert_eq!(
        frozen_reading,
        paused_wall.saturating_sub(25) + c.offset_secs.load(Ordering::SeqCst)
    );

    // If paused_at_wall_secs is 0 while paused, falls back to host_wall_unix_secs_and_nanos
    c.paused_at_wall_secs.store(0, Ordering::SeqCst);
    assert!(c.now_unix_secs() >= frozen_reading);
    c.paused_at_wall_secs
        .store(paused_wall.saturating_sub(25), Ordering::SeqCst);

    // Unpausing accumulates the 25s into paused_accum_wall_secs
    c.set_paused(false);
    assert!(!c.is_paused());
    assert_eq!(c.paused_at_wall_secs.load(Ordering::SeqCst), 0);
    assert!(c.paused_accum_wall_secs.load(Ordering::SeqCst) >= 25);
}
