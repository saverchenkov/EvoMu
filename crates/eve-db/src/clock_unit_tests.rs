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
    assert_eq!(c.offset_secs(), 0);

    // 4x tick_wall_second on wall-anchored clock adds (4 - 1) = 3 offset seconds
    c.set_acceleration_multiplier(4);
    assert_eq!(c.tick_wall_second(), 4);
    assert_eq!(c.offset_secs(), 3);

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
        paused_wall.saturating_sub(25) + c.offset_secs()
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

#[test]
fn virtual_clock_offset_construct_restore_and_advance_helpers() {
    let wall_with_off = VirtualServerClock::with_offset_secs(3_600);
    assert_eq!(wall_with_off.offset_secs(), 3_600);
    let trait_ref: &dyn ServerClock = &wall_with_off;
    assert_eq!(trait_ref.offset_secs(), 3_600);

    let fixed = VirtualServerClock::with_fixed_base_and_offset(1_760_000_000, 120);
    assert_eq!(fixed.offset_secs(), 120);
    assert_eq!(fixed.now_unix_secs(), 1_760_000_120);

    let new_off = fixed.advance_secs_and_read_offset(480);
    assert_eq!(new_off, 600);
    assert_eq!(fixed.offset_secs(), 600);
    assert_eq!(fixed.now_unix_secs(), 1_760_000_600);

    let fixed_trait: &dyn ServerClock = &fixed;
    fixed_trait.restore_offset_secs(86_400);
    assert_eq!(fixed.offset_secs(), 86_400);
    assert_eq!(fixed.now_unix_secs(), 1_760_086_400);

    let dummy = DummyTraitClock {
        secs: 1_700_000_000,
        nanos: 0,
        stepped: AtomicU64::new(0),
    };
    assert_eq!(dummy.offset_secs(), 0);
    dummy.restore_offset_secs(999);
    assert_eq!(dummy.offset_secs(), 0);
}

#[test]
fn sqlite_tx_persists_rolls_back_and_restores_virtual_clock_offset() {
    let dir = std::env::temp_dir().join(format!(
        "evomu_clock_offset_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let db_path = dir.join("world.sqlite");
    let export_path = dir.join("export.sqlite");

    {
        let repo = crate::WorldRepository::open_sqlite(&db_path).expect("open_sqlite");
        assert_eq!(repo.clock().offset_secs(), 0);

        repo.tx("clock_offset_commit", |wtx| -> Result<(), crate::TxError> {
            wtx.set_clock_offset_secs(12_345);
            Ok(())
        })
        .expect("commit clock offset");

        // Aborted transaction must roll back world_meta.clock_offset_secs cleanly.
        let aborted: Result<(), crate::TxError> = repo.tx("clock_offset_abort", |wtx| {
            wtx.set_clock_offset_secs(99_999);
            Err(crate::TxError::Persist("simulated abort".into()))
        });
        assert!(aborted.is_err());

        // Fault injection at commit must also roll back world_meta.clock_offset_secs atomically.
        repo.set_fault_plan_for_test(crate::TxFaultPlan::FailAtCommit);
        let faulted: Result<(), crate::TxError> = repo.tx("clock_offset_fault_commit", |wtx| {
            wtx.snap.next_entity_id += 1;
            wtx.set_clock_offset_secs(88_888);
            Ok(())
        });
        assert!(faulted.is_err());
    }

    let reopened = crate::WorldRepository::open_sqlite(&db_path).expect("reopen_sqlite");
    assert_eq!(reopened.clock().offset_secs(), 12_345);

    reopened.clock().restore_offset_secs(54_321);
    reopened.save_to_sqlite(&export_path).expect("save_to_sqlite");
    let exported = crate::WorldRepository::open_sqlite(&export_path).expect("open exported");
    assert_eq!(exported.clock().offset_secs(), 54_321);

    drop(exported);
    drop(reopened);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn sqlite_tx_persists_rolls_back_and_exports_unrebased_clock_offset() {
    let dir = std::env::temp_dir().join(format!(
        "evomu_unrebased_clock_offset_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock after epoch")
            .as_nanos()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create temp dir");
    let db_path = dir.join("world.sqlite");
    let export_path = dir.join("export.sqlite");

    {
        let repo = crate::WorldRepository::open_sqlite(&db_path).expect("open_sqlite");
        assert_eq!(
            repo.persisted_unrebased_clock_offset_secs()
                .expect("read initial unrebased offset"),
            0
        );

        repo.tx("unrebased_commit", |wtx| -> Result<(), crate::TxError> {
            wtx.set_clock_offset_secs(12_345);
            wtx.set_unrebased_clock_offset_secs(10_000);
            Ok(())
        })
        .expect("commit clock offsets");

        let aborted: Result<(), crate::TxError> = repo.tx("unrebased_abort", |wtx| {
            wtx.set_clock_offset_secs(99_999);
            wtx.set_unrebased_clock_offset_secs(88_888);
            Err(crate::TxError::Persist("simulated abort".into()))
        });
        assert!(aborted.is_err());

        repo.set_fault_plan_for_test(crate::TxFaultPlan::FailAtCommit);
        let faulted: Result<(), crate::TxError> = repo.tx("unrebased_fault_commit", |wtx| {
            wtx.snap.next_entity_id += 1;
            wtx.set_clock_offset_secs(77_777);
            wtx.set_unrebased_clock_offset_secs(66_666);
            Ok(())
        });
        assert!(faulted.is_err());
    }

    let reopened = crate::WorldRepository::open_sqlite(&db_path).expect("reopen_sqlite");
    assert_eq!(reopened.clock().offset_secs(), 12_345);
    assert_eq!(
        reopened
            .persisted_unrebased_clock_offset_secs()
            .expect("read persisted unrebased offset"),
        10_000
    );

    reopened.save_to_sqlite(&export_path).expect("save_to_sqlite");
    let exported = crate::WorldRepository::open_sqlite(&export_path).expect("open exported");
    assert_eq!(exported.clock().offset_secs(), 12_345);
    assert_eq!(
        exported
            .persisted_unrebased_clock_offset_secs()
            .expect("read exported unrebased offset"),
        10_000
    );

    drop(exported);
    drop(reopened);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn virtual_clock_advance_millis_and_set_wall_now_for_tests() {
    let clock = VirtualServerClock::with_fixed_base(1_760_000_000);
    assert_eq!(clock.now_unix_secs(), 1_760_000_000);
    assert_eq!(clock.now_unix_millis(), 1_760_000_000_000);

    clock.advance_millis(450);
    assert_eq!(clock.now_unix_secs(), 1_760_000_000);
    assert_eq!(clock.now_subsec_nanos(), 450_000_000);
    assert_eq!(clock.now_unix_millis(), 1_760_000_000_450);

    clock.advance_millis(600);
    assert_eq!(clock.now_unix_secs(), 1_760_000_001);
    assert_eq!(clock.now_subsec_nanos(), 50_000_000);
    assert_eq!(clock.offset_secs(), 1);

    let dyn_clock: &dyn ServerClock = &clock;
    dyn_clock.set_wall_now_for_tests(1_800_000_000);
    assert_eq!(dyn_clock.now_unix_secs(), 1_800_000_000);
    assert_eq!(dyn_clock.now_subsec_nanos(), 0);

    dyn_clock.set_wall_now_ms_for_tests(1_800_000_123_456);
    assert_eq!(dyn_clock.now_unix_secs(), 1_800_000_123);
    assert_eq!(dyn_clock.now_subsec_nanos(), 456_000_000);
}

mod clock_domain_unit_tests {
    use crate::clock::{FILETIME_TICKS_PER_MS, FILETIME_UNIX_EPOCH_DIFF_MS};
    use crate::{
        DestinyStamp, SimDilatedMs, SimFileTime, SolarSystemSimClock, WallFileTime, WallUnixMs,
        WorldStateSnapshot,
    };
    use std::any::TypeId;

    #[test]
    fn clock_domain_types_conversions_and_checked_bounds() {
        assert_ne!(TypeId::of::<WallUnixMs>(), TypeId::of::<SimDilatedMs>());
        assert_ne!(TypeId::of::<WallFileTime>(), TypeId::of::<SimFileTime>());

        let unix_ms = WallUnixMs::from_millis(1_760_000_000_123);
        assert_eq!(unix_ms.as_millis(), 1_760_000_000_123);
        assert_eq!(unix_ms.as_secs(), 1_760_000_000);
        assert_eq!(
            WallUnixMs::checked_from_secs(1_760_000_000),
            Some(WallUnixMs::from_millis(1_760_000_000_000))
        );
        assert_eq!(WallUnixMs::checked_from_secs(u64::MAX), None);

        let wall_ft = unix_ms
            .to_wall_filetime()
            .expect("unix timestamp converts to WallFileTime");
        let expected_wall_ticks =
            (1_760_000_000_123_i64 + FILETIME_UNIX_EPOCH_DIFF_MS) * FILETIME_TICKS_PER_MS;
        assert_eq!(wall_ft, WallFileTime::from_ticks(expected_wall_ticks));
        assert_eq!(wall_ft.to_wall_unix_ms(), Some(unix_ms));

        let epoch_ft = WallUnixMs::from_millis(0)
            .to_wall_filetime()
            .expect("unix epoch converts to WallFileTime");
        assert_eq!(epoch_ft.to_wall_unix_ms(), Some(WallUnixMs::from_millis(0)));
        assert_eq!(
            epoch_ft
                .checked_sub_ticks(1)
                .and_then(WallFileTime::to_wall_unix_ms),
            None
        );
        assert_eq!(WallFileTime::from_ticks(i64::MIN).to_wall_unix_ms(), None);

        assert_eq!(WallUnixMs::from_millis(u64::MAX).to_wall_filetime(), None);
        assert_eq!(WallUnixMs::from_millis(u64::MAX).checked_add_ms(1), None);
        assert_eq!(WallUnixMs::from_millis(10).checked_sub_ms(11), None);
        assert_eq!(
            WallUnixMs::from_millis(10).checked_elapsed_since(WallUnixMs::from_millis(11)),
            None
        );

        let sim_ms = SimDilatedMs::from_millis(1_759_999_700_450);
        let sim_ft = sim_ms
            .to_sim_filetime()
            .expect("sim ms converts to SimFileTime");
        let expected_sim_ticks =
            (1_759_999_700_450_i64 + FILETIME_UNIX_EPOCH_DIFF_MS) * FILETIME_TICKS_PER_MS;
        assert_eq!(sim_ft, SimFileTime::from_ticks(expected_sim_ticks));
        assert_eq!(sim_ft.to_sim_dilated_ms(), Some(sim_ms));
        assert_eq!(sim_ms.subsec_ms(), 450);
        assert_eq!(sim_ms.remaining_ms_in_tick(), 550);
        assert_eq!(
            sim_ms.checked_to_destiny_stamp(),
            Some(DestinyStamp::from_ticks(1_759_999_700))
        );
        assert_eq!(SimDilatedMs::from_millis(u64::MAX).to_sim_filetime(), None);
        assert_eq!(
            SimDilatedMs::from_millis(u64::MAX).checked_to_destiny_stamp(),
            None
        );
        assert_eq!(
            SimDilatedMs::from_millis(u64::MAX).to_destiny_stamp_saturating(),
            DestinyStamp::from_ticks(u32::MAX)
        );
        assert_eq!(SimFileTime::from_ticks(i64::MIN).to_sim_dilated_ms(), None);
    }

    #[test]
    fn solar_system_sim_clock_nonzero_initial_offset_and_projection() {
        let wall_0 = WallUnixMs::from_millis(1_760_000_000_000);
        let clock = SolarSystemSimClock::new(wall_0, -300_000, DestinyStamp::from_ticks(10_000), 1.0)
            .expect("valid initial offset builds clock");

        assert_eq!(clock.last_wall_ms(), wall_0);
        assert_eq!(
            clock.sim_dilated_ms(),
            SimDilatedMs::from_millis(1_759_999_700_000)
        );
        assert_eq!(clock.sim_minus_wall_offset_ms(), Some(-300_000));
        assert_eq!(clock.tidi_factor(), 1.0);
        assert_eq!(clock.destiny_stamp(), DestinyStamp::from_ticks(10_000));

        let wall_ft = clock.last_wall_filetime().expect("wall filetime");
        assert_ne!(clock.sim_filetime().as_ticks(), wall_ft.as_ticks());
        assert_eq!(
            wall_ft.as_ticks() - clock.sim_filetime().as_ticks(),
            300_000 * FILETIME_TICKS_PER_MS
        );

        let wall_plus_5s = wall_0.checked_add_ms(5_000).expect("wall + 5s");
        assert_eq!(
            clock.project_wall_to_sim_dilated_ms(wall_plus_5s),
            Some(SimDilatedMs::from_millis(1_759_999_705_000))
        );
        assert_eq!(
            clock.project_wall_to_sim_filetime(wall_plus_5s),
            SimDilatedMs::from_millis(1_759_999_705_000).to_sim_filetime()
        );
        assert_eq!(
            clock.project_wall_to_sim_dilated_ms(WallUnixMs::from_millis(1_759_999_999_000)),
            None
        );

        // Positive offset also supported without overflow.
        let pos_clock =
            SolarSystemSimClock::new(wall_0, 45_500, DestinyStamp::from_ticks(1), 1.0)
                .expect("positive initial offset succeeds");
        assert_eq!(pos_clock.sim_minus_wall_offset_ms(), Some(45_500));

        // Checked validation failures return None without panicking.
        assert_eq!(
            SolarSystemSimClock::new(
                WallUnixMs::from_millis(100),
                -500,
                DestinyStamp::from_ticks(1),
                1.0
            ),
            None
        );
        assert_eq!(
            SolarSystemSimClock::new(wall_0, 0, DestinyStamp::from_ticks(1), f64::NAN),
            None
        );
        assert_eq!(SolarSystemSimClock::clamp_tidi_factor(0.01), Some(0.10));
        assert_eq!(SolarSystemSimClock::clamp_tidi_factor(2.5), Some(1.0));
        assert_eq!(SolarSystemSimClock::clamp_tidi_factor(f64::INFINITY), None);
    }

    #[test]
    fn solar_system_sim_clock_tidi_step_transitions_monotonic_and_stamp_tracking() {
        let wall_0 = WallUnixMs::from_millis(1_760_000_000_000);
        let mut clock =
            SolarSystemSimClock::new(wall_0, -120_000, DestinyStamp::from_ticks(500), 1.0)
                .expect("clock initialized");
        let ft_0 = clock.sim_filetime();

        // Phase 1 (TiDi 1.0): advance 1,500 wall ms -> +1,500 sim ms (stamp 500 -> 501, rem 500 ms).
        let wall_1 = wall_0.checked_add_ms(1_500).expect("wall_1");
        let sim_1 = clock.advance_to_wall_ms(wall_1).expect("phase 1 advance");
        let ft_1 = clock.sim_filetime();
        assert_eq!(sim_1, SimDilatedMs::from_millis(1_759_999_881_500));
        assert!(ft_1 > ft_0);
        assert_eq!(clock.destiny_stamp(), DestinyStamp::from_ticks(501));
        assert_eq!(clock.stamp_remainder_ms(), 500);
        assert_eq!(clock.remaining_ms_in_tick(), 500);
        assert_eq!(clock.sim_minus_wall_offset_ms(), Some(-120_000));

        // Transition 1.0 -> 0.25 at wall_1: SimFileTime and stamp remain monotonic.
        assert_eq!(clock.set_tidi_factor_at(wall_1, 0.25), Some(0.25));
        assert_eq!(clock.sim_filetime(), ft_1);
        assert_eq!(clock.destiny_stamp(), DestinyStamp::from_ticks(501));
        assert_eq!(clock.wall_ms_needed_for_sim_ms(500), Some(2_000));

        // Phase 2 (TiDi 0.25): advance 2,000 wall ms -> +500 sim ms -> reaches stamp 502 cleanly.
        let wall_2 = wall_1.checked_add_ms(2_000).expect("wall_2");
        let sim_2 = clock.advance_to_wall_ms(wall_2).expect("phase 2 advance");
        let ft_2 = clock.sim_filetime();
        assert_eq!(sim_2, SimDilatedMs::from_millis(1_759_999_882_000));
        assert!(ft_2 >= ft_1);
        assert_eq!(clock.destiny_stamp(), DestinyStamp::from_ticks(502));
        assert_eq!(clock.stamp_remainder_ms(), 0);
        assert_eq!(clock.remaining_ms_in_tick(), 1_000);
        assert_eq!(clock.sim_minus_wall_offset_ms(), Some(-121_500));

        // Transition 0.25 -> 0.8 after +1,200 wall ms (+300 sim ms under outgoing 0.25).
        let wall_3 = wall_2.checked_add_ms(1_200).expect("wall_3");
        assert_eq!(clock.set_tidi_factor_at(wall_3, 0.8), Some(0.8));
        let ft_3 = clock.sim_filetime();
        assert!(ft_3 >= ft_2);
        assert_eq!(clock.destiny_stamp(), DestinyStamp::from_ticks(502));
        assert_eq!(clock.stamp_remainder_ms(), 300);
        assert_eq!(clock.remaining_ms_in_tick(), 700);

        // Phase 3 (TiDi 0.8): advance 1,000 wall ms -> +800 sim ms (300 + 800 = 1100 -> stamp 503, rem 100).
        let wall_4 = wall_3.checked_add_ms(1_000).expect("wall_4");
        let sim_4 = clock.advance_to_wall_ms(wall_4).expect("phase 3 advance");
        let ft_4 = clock.sim_filetime();
        assert_eq!(sim_4, SimDilatedMs::from_millis(1_759_999_883_100));
        assert!(ft_4 > ft_3);
        assert_eq!(clock.destiny_stamp(), DestinyStamp::from_ticks(503));
        assert_eq!(clock.stamp_remainder_ms(), 100);
        assert_eq!(clock.remaining_ms_in_tick(), 900);

        // Transition 0.8 -> 1.0 at wall_4, then +900 wall ms -> reaches stamp 504 with 0 remainder.
        assert_eq!(clock.set_tidi_factor_at(wall_4, 1.0), Some(1.0));
        let wall_5 = wall_4.checked_add_ms(900).expect("wall_5");
        let sim_5 = clock.advance_to_wall_ms(wall_5).expect("phase 4 advance");
        let ft_5 = clock.sim_filetime();
        assert_eq!(sim_5, SimDilatedMs::from_millis(1_759_999_884_000));
        assert!(ft_5 > ft_4);
        assert_eq!(clock.destiny_stamp(), DestinyStamp::from_ticks(504));
        assert_eq!(clock.stamp_remainder_ms(), 0);
        assert_eq!(clock.remaining_ms_in_tick(), 1_000);

        // Backward wall timestamp never moves SimFileTime or DestinyStamp backward.
        let backward_wall = wall_0;
        assert_eq!(clock.advance_to_wall_ms(backward_wall), Some(sim_5));
        assert_eq!(clock.sim_filetime(), ft_5);
        assert_eq!(clock.destiny_stamp(), DestinyStamp::from_ticks(504));

        // Non-finite TiDi change and overflowing delta fail safely without mutating state.
        assert_eq!(clock.set_tidi_factor(f64::NAN), None);
        assert_eq!(clock.advance_wall_ms(u64::MAX), None);
        assert_eq!(clock.sim_filetime(), ft_5);
        assert_eq!(clock.tidi_factor(), 1.0);

        // Fixed-point precision avoids IEEE-754 floor drift on 100ms * 0.29 = 29ms.
        let mut drift_clock =
            SolarSystemSimClock::new(wall_0, 0, DestinyStamp::from_ticks(1), 0.29)
                .expect("0.29 TiDi clock");
        let after_100ms = drift_clock
            .advance_wall_ms(100)
            .expect("100ms advance at 0.29 TiDi");
        assert_eq!(
            after_100ms,
            SimDilatedMs::from_millis(wall_0.as_millis() + 29)
        );
    }

    #[test]
    fn snapshot_now_unix_secs_parses_virtual_clock_blob() {
        let mut snap = WorldStateSnapshot::default();
        assert_eq!(snap.checked_snapshot_now_unix_secs(), None);
        assert_eq!(snap.snapshot_now_unix_secs(), 0);

        snap.service_state_blobs
            .insert("__server_now_unix".into(), "1760000123".into());
        assert_eq!(snap.checked_snapshot_now_unix_secs(), Some(1_760_000_123));
        assert_eq!(snap.snapshot_now_unix_secs(), 1_760_000_123);

        snap.service_state_blobs
            .insert("__server_now_unix".into(), " 2000.75 ".into());
        assert_eq!(snap.checked_snapshot_now_unix_secs(), Some(2_000));
        assert_eq!(snap.snapshot_now_unix_secs(), 2_000);

        snap.service_state_blobs
            .insert("__server_now_unix".into(), "-5".into());
        assert_eq!(snap.checked_snapshot_now_unix_secs(), None);
        assert_eq!(snap.snapshot_now_unix_secs(), 0);

        snap.service_state_blobs
            .insert("__server_now_unix".into(), "NaN".into());
        assert_eq!(snap.checked_snapshot_now_unix_secs(), None);
        assert_eq!(snap.snapshot_now_unix_secs(), 0);
    }
}
