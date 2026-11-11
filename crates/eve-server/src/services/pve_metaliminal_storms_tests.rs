//! Unit tests for `pve_metaliminal_storms`.

use super::*;
use crate::router::ServerContext;
use eve_db::clock::VirtualServerClock;
use eve_db::WorldRepository;
use eve_sde::{SdeRegistry, SolarSystemId};
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed()))
        .clone()
}

fn find_nullsec_system(sde: &SdeRegistry) -> SolarSystemId {
    let mut cands: Vec<SolarSystemId> = sde
        .solar_systems
        .values()
        .filter(|s| is_nullsec_storm_system(sde, s.solar_system_id))
        .map(|s| s.solar_system_id)
        .collect();
    cands.sort_unstable();
    cands.into_iter().next().unwrap_or(30_000_208)
}

#[test]
fn storm_beacons_footprint_movement_and_anomalies() {
    let sde = shared_sde();
    let ns_sys = find_nullsec_system(&sde);
    assert!(is_nullsec_storm_system(&sde, ns_sys));
    assert!(!is_nullsec_storm_system(&sde, 31_000_001));
    assert!(!is_nullsec_storm_system(&sde, 99_999_999));

    for (w, expected) in [
        (AbyssalWeatherKind::Electrical, (47381, 47382)),
        (AbyssalWeatherKind::Exotic, (47384, 47385)),
        (AbyssalWeatherKind::Gamma, (47387, 47388)),
        (AbyssalWeatherKind::Firestorm, (47390, 47391)),
        (AbyssalWeatherKind::Dark, (47378, 47379)),
    ] {
        let pair = storm_weather_beacons(w);
        assert_eq!(pair, expected);
        assert!(is_storm_beacon_type(pair.0));
        assert!(is_storm_beacon_type(pair.1));
    }
    assert!(!is_storm_beacon_type(34));

    let db = Arc::new(WorldRepository::new_empty());
    let ctx = ServerContext::with_sde_and_repository(sde, db);
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_775_500_000)));

    let (core, periphery) = compute_storm_footprint(&ctx, ns_sys);
    assert!(core.contains(&ns_sys));
    assert!(periphery.iter().all(|p| !core.contains(p)));

    for step in 0..5u32 {
        let interval = storm_move_interval_secs(1, step);
        assert!((STORM_MOVE_MIN_SECS..=STORM_MOVE_MAX_SECS).contains(&interval));
    }

    with_pve_special_reg(&ctx, |reg| {
        init_roaming_storms(&ctx, reg);
        assert_eq!(reg.roaming_storms.len(), 4);
        for s in &mut reg.roaming_storms {
            s.next_move_at_sec = 1_775_500_000;
        }
    });
    sync_metaliminal_storm_effects_and_sites(&ctx);

    let initial_centers: Vec<SolarSystemId> = with_pve_special_reg(&ctx, |reg| {
        reg.roaming_storms.iter().map(|s| s.center_solar_system_id).collect()
    });
    ctx.db.with_snapshot(|snap| {
        let has_volatile_ice = snap
            .cosmic_anomalies
            .values()
            .flatten()
            .any(|a| a.anomaly_id.starts_with("DNG-9042-") && a.ore_types.contains(&56142));
        assert!(has_volatile_ice, "storm core spawns Volatile Ice anomaly");
    });

    with_pve_special_reg(&ctx, |reg| {
        reg.sim_clock_sec = 1_775_700_000;
        advance_roaming_storms(&ctx, reg, 200_000);
        for (idx, s) in reg.roaming_storms.iter().enumerate() {
            if !nullsec_neighbors(&ctx.sde, initial_centers[idx]).is_empty() {
                assert!(s.step_count >= 1);
            }
        }
    });
    sync_metaliminal_storm_effects_and_sites(&ctx);

    let list = get_all_roaming_weather_systems(&ctx);
    assert!(!list.is_empty());
}

#[test]
fn nullsec_storm_system_filters_pick_neighbor_and_lifecycle_notifications() {
    let mut sde = (*shared_sde()).clone();
    // Create a controlled 4-system nullsec chain (< 30_100_000): 30_090_001 <-> 30_090_002 <-> 30_090_003 <-> 30_090_004
    // Plus an isolated nullsec system 30_090_010, a Jove system (region 10_000_004), and a WH class 3 system.
    let base_sys = sde.solar_systems.values().next().unwrap().clone();
    for (id, sec, region, wh) in [
        (30_090_001_u32, -0.5_f64, 10_000_001_u32, None),
        (30_090_002, -0.6, 10_000_001, Some(8)),
        (30_090_003, -0.7, 10_000_001, Some(9)),
        (30_090_004, -0.8, 10_000_001, None),
        (30_090_010, -0.9, 10_000_001, None), // isolated
        (30_090_011, -0.9, 10_000_004, None), // Jove excluded
        (30_090_012, -0.9, 10_000_001, Some(3)), // WH class 3 excluded
        (30_090_013, 0.2, 10_000_001, None),  // lowsec excluded
    ] {
        let mut sys = base_sys.clone();
        sys.solar_system_id = id;
        sys.security_status = sec;
        sys.region_id = region;
        sys.wormhole_class_id = wh;
        sde.solar_systems.insert(id, sys);
    }
    let mut next_jid = sde.jumps.keys().copied().max().unwrap_or(1) + 1;
    for (a, b) in [
        (30_090_001, 30_090_002),
        (30_090_002, 30_090_001),
        (30_090_002, 30_090_003),
        (30_090_003, 30_090_002),
        (30_090_003, 30_090_004),
        (30_090_004, 30_090_003),
    ] {
        sde.jumps.insert(
            next_jid,
            eve_sde::StargateJump {
                stargate_id: next_jid,
                from_solar_system_id: a,
                to_solar_system_id: b,
                destination_stargate_id: next_jid + 1000,
                position: glam::DVec3::ZERO,
            },
        );
        next_jid += 1;
    }

    assert!(is_nullsec_storm_system(&sde, 30_090_001));
    assert!(is_nullsec_storm_system(&sde, 30_090_002));
    assert!(!is_nullsec_storm_system(&sde, 30_090_011));
    assert!(!is_nullsec_storm_system(&sde, 30_090_012));
    assert!(!is_nullsec_storm_system(&sde, 30_090_013));
    assert!(!is_nullsec_storm_system(&sde, 30_100_000));

    // pick_non_retracing_neighbor:
    // 1) isolated -> None
    let iso = RoamingStormRecord {
        storm_id: 99,
        weather: AbyssalWeatherKind::Dark,
        center_solar_system_id: 30_090_010,
        strong_core_systems: vec![30_090_010],
        weak_periphery_systems: vec![],
        step_count: 0,
        next_move_at_sec: 100,
        recent_centers: vec![30_090_010],
    };
    assert_eq!(pick_non_retracing_neighbor(&sde, &iso), None);

    // 2) prefers non-recent neighbor (from 30_090_002 with recent=[30_090_001, 30_090_002] -> picks 30_090_003)
    let mut chain = RoamingStormRecord {
        storm_id: 1,
        weather: AbyssalWeatherKind::Gamma,
        center_solar_system_id: 30_090_002,
        strong_core_systems: vec![30_090_002, 30_090_001, 30_090_003],
        weak_periphery_systems: vec![30_090_004],
        step_count: 1,
        next_move_at_sec: 100,
        recent_centers: vec![30_090_001, 30_090_002],
    };
    assert_eq!(pick_non_retracing_neighbor(&sde, &chain), Some(30_090_003));

    // 3) when all neighbors are in recent_centers, avoids immediate prev_center (30_090_001) -> picks 30_090_003
    chain.recent_centers = vec![30_090_003, 30_090_001, 30_090_002];
    assert_eq!(pick_non_retracing_neighbor(&sde, &chain), Some(30_090_003));

    // 4) when only neighbor is prev_center (30_090_001 has only 30_090_002), falls back to candidates
    let dead_end = RoamingStormRecord {
        storm_id: 2,
        weather: AbyssalWeatherKind::Exotic,
        center_solar_system_id: 30_090_001,
        strong_core_systems: vec![],
        weak_periphery_systems: vec![],
        step_count: 1,
        next_move_at_sec: 100,
        recent_centers: vec![30_090_002, 30_090_001],
    };
    assert_eq!(pick_non_retracing_neighbor(&sde, &dead_end), Some(30_090_002));

    // Test advance_roaming_storms with next_move_at_sec == 0, empty recent_centers, and recent_centers truncation (> 8)
    let db = Arc::new(WorldRepository::new_empty());
    let ctx = ServerContext::with_sde_and_repository(Arc::new(sde), db);
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_800_000_000)));

    let mut reg = PveSpecialSitesRegistry::default();
    reg.sim_clock_sec = 1_800_000_000;
    reg.roaming_storms.push(RoamingStormRecord {
        storm_id: 7,
        weather: AbyssalWeatherKind::Dark,
        center_solar_system_id: 30_090_002,
        strong_core_systems: vec![30_090_002],
        weak_periphery_systems: vec![30_090_001],
        step_count: 0,
        next_move_at_sec: 0,
        recent_centers: Vec::new(),
    });
    // First call initializes next_move_at_sec and recent_centers without stepping
    advance_roaming_storms(&ctx, &mut reg, 0);
    assert_eq!(reg.roaming_storms[0].recent_centers, vec![30_090_002]);
    assert_eq!(reg.roaming_storms[0].step_count, 0);
    assert!(reg.roaming_storms[0].next_move_at_sec > 1_800_000_000);

    // init_roaming_storms is a no-op when roaming_storms is non-empty
    init_roaming_storms(&ctx, &mut reg);
    assert_eq!(reg.roaming_storms.len(), 1);

    // Advance far in future -> caps at 8 steps and trims recent_centers to <= 8
    reg.sim_clock_sec = 1_900_000_000;
    reg.roaming_storms[0].recent_centers = vec![1, 2, 3, 4, 5, 6, 7, 30_090_002];
    advance_roaming_storms(&ctx, &mut reg, 100_000_000);
    assert_eq!(reg.roaming_storms[0].step_count, 8);
    assert!(reg.roaming_storms[0].recent_centers.len() <= 8);

    // Test sync_metaliminal_storm_effects_and_sites cleanup & stop notifications when a storm moves away or disappears
    // Seed a pilot in 30_090_001 so we observe OnSystemEffectStarted, OnAttributes, OnSystemWideEffectStop, OnComponentRemoved
    ctx.db.provision_gm_account("stormwatcher", "pw", 91_000_001, "StormWatcher");
    let watcher_uid = ctx.db.with_snapshot(|s| s.characters[&91_000_001].user_id);
    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&91_000_001).expect("provisioned stormwatcher");
        ch.solar_system_id = 30_090_001;
        ch.station_id = None;
        ch.structure_id = None;
        ch.active_ship_id = 1_090_000_001;
    });
    ctx.db
        .acquire_online_lock(watcher_uid, 91_001, Some(91_000_001), true, ctx.now_unix())
        .expect("acquire stormwatcher online lock");
    with_pve_special_reg(&ctx, |r| {
        r.roaming_storms = vec![RoamingStormRecord {
            storm_id: 10,
            weather: AbyssalWeatherKind::Dark,
            center_solar_system_id: 30_090_001,
            strong_core_systems: vec![30_090_001],
            // Include 30_090_001 in periphery too to verify core overrides periphery!
            weak_periphery_systems: vec![30_090_001, 30_090_002],
            step_count: 0,
            next_move_at_sec: 1_900_000_000,
            recent_centers: vec![30_090_001],
        }];
    });
    sync_metaliminal_storm_effects_and_sites(&ctx);
    let started_notes = ctx.drain_notifications_for_char(91_000_001);
    assert!(started_notes.iter().any(|n| n.event == "OnSystemEffectStarted"));
    assert!(started_notes.iter().any(|n| n.event == "OnAttributes"));

    // Verify 30_090_001 got ONLY strong Dark beacon (47379), not weak (47378), and got core anomalies
    ctx.db.with_snapshot(|snap| {
        let beacons: Vec<u32> = snap
            .items
            .values()
            .filter(|i| i.location_id == 30_090_001 && is_storm_beacon_type(i.type_id))
            .map(|i| i.type_id)
            .collect();
        assert_eq!(beacons, vec![47379]);
        let anoms = &snap.cosmic_anomalies[&30_090_001];
        assert!(anoms.iter().any(|a| a.anomaly_id == "DNG-9042-30090001"));
        assert!(anoms.iter().any(|a| a.anomaly_id == "DNG-9043-30090001"));
    });

    // Move storm completely away -> removes beacons + anomalies in 30_090_001 and emits stop/removed notifications
    with_pve_special_reg(&ctx, |r| {
        r.roaming_storms.clear();
    });
    sync_metaliminal_storm_effects_and_sites(&ctx);
    let stop_notes = ctx.drain_notifications_for_char(91_000_001);
    assert!(stop_notes.iter().any(|n| n.event == "OnSystemWideEffectStop"));
    assert!(stop_notes
        .iter()
        .any(|n| n.event == "OnComponentRemovedSystemEnvironment"));
    assert!(stop_notes.iter().any(|n| n.event == "OnAttributes"));
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.items.values().any(|i| is_storm_beacon_type(i.type_id)));
        assert!(snap
            .cosmic_anomalies
            .get(&30_090_001)
            .is_none_or(|v| v.is_empty()));
    });
}
