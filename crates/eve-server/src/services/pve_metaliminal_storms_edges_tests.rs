#![forbid(unsafe_code)]

use super::*;
use crate::router::ServerContext;
use eve_db::clock::VirtualServerClock;
use eve_db::{CosmicAnomalyRecord, InventoryFlag, InventoryItemRecord, WorldRepository};
use eve_sde::{SdeRegistry, SolarSystem, StargateJump};
use std::sync::Arc;

fn make_nullsec_sde_ring() -> SdeRegistry {
    let mut sde = SdeRegistry::default();
    let add_sys = |sde: &mut SdeRegistry, id: u32, sec: f64, reg: u32, wh: Option<i32>| {
        sde.solar_systems.insert(
            id,
            SolarSystem {
                solar_system_id: id,
                constellation_id: 20_000_001,
                region_id: reg,
                name: format!("Sys-{id}"),
                security_status: sec,
                radius: 1.0e12,
                galactic_pos_ly: [0.0; 3],
                planet_ids: vec![],
                stations: vec![],
                wormhole_class_id: wh,
            },
        );
    };
    // 30_000_208 (center) <-> 30_000_209 <-> 30_000_210 (dead-end leaf), plus highsec 30_000_142 & forbidden region 10_000_004
    add_sys(&mut sde, 30_000_208, -0.5, 10_000_002, None);
    add_sys(&mut sde, 30_000_209, -0.4, 10_000_002, None);
    add_sys(&mut sde, 30_000_210, -0.6, 10_000_002, None);
    add_sys(&mut sde, 30_000_211, -0.7, 10_000_002, None); // isolated
    add_sys(&mut sde, 30_000_142, 0.9, 10_000_002, None); // highsec
    add_sys(&mut sde, 30_000_212, -0.5, 10_000_004, None); // excluded region

    let mut jid = 1_i64;
    for (a, b) in [
        (30_000_208, 30_000_209),
        (30_000_209, 30_000_208),
        (30_000_209, 30_000_210),
        (30_000_210, 30_000_209),
        (30_000_208, 30_000_142),
        (30_000_208, 30_000_212),
    ] {
        sde.jumps.insert(
            jid,
            StargateJump {
                stargate_id: jid,
                from_solar_system_id: a,
                to_solar_system_id: b,
                destination_stargate_id: jid + 100,
                position: glam::DVec3::ZERO,
            },
        );
        jid += 1;
    }
    sde
}

#[test]
fn storms_pick_non_retracing_neighbor_and_advance_edge_cases() {
    let sde = Arc::new(make_nullsec_sde_ring());
    let db = Arc::new(WorldRepository::new_empty());
    let ctx = ServerContext::with_sde_and_repository(sde, db);
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_800_000_000)));

    assert!(is_nullsec_storm_system(&ctx.sde, 30_000_208));
    assert!(!is_nullsec_storm_system(&ctx.sde, 30_000_142));
    assert!(!is_nullsec_storm_system(&ctx.sde, 30_000_212));

    // Isolated system -> None
    let isolated = RoamingStormRecord {
        storm_id: 10,
        weather: AbyssalWeatherKind::Dark,
        center_solar_system_id: 30_000_211,
        strong_core_systems: vec![30_000_211],
        weak_periphery_systems: vec![],
        step_count: 0,
        next_move_at_sec: 0,
        recent_centers: vec![],
    };
    assert_eq!(pick_non_retracing_neighbor(&ctx.sde, &isolated), None);

    // Dead-end system 30_000_210 with only neighbor 30_000_209 already as prev_center -> falls back to candidates
    let dead_end = RoamingStormRecord {
        storm_id: 11,
        weather: AbyssalWeatherKind::Electrical,
        center_solar_system_id: 30_000_210,
        strong_core_systems: vec![30_000_210, 30_000_209],
        weak_periphery_systems: vec![30_000_208],
        step_count: 2,
        next_move_at_sec: 0,
        recent_centers: vec![30_000_209, 30_000_210],
    };
    assert_eq!(pick_non_retracing_neighbor(&ctx.sde, &dead_end), Some(30_000_209));

    // Hub 30_000_209 with both neighbors in recent_centers, prev_center = 30_000_208 -> picks non_prev 30_000_210
    let hub = RoamingStormRecord {
        storm_id: 12,
        weather: AbyssalWeatherKind::Gamma,
        center_solar_system_id: 30_000_209,
        strong_core_systems: vec![],
        weak_periphery_systems: vec![],
        step_count: 3,
        next_move_at_sec: 0,
        recent_centers: vec![30_000_210, 30_000_208, 30_000_209],
    };
    assert_eq!(pick_non_retracing_neighbor(&ctx.sde, &hub), Some(30_000_210));

    // Empty recent_centers on 30_000_209 -> picks from candidates
    let no_recent = RoamingStormRecord {
        recent_centers: vec![],
        ..hub.clone()
    };
    assert!(pick_non_retracing_neighbor(&ctx.sde, &no_recent).is_some());

    // advance_roaming_storms: initializes empty recent_centers and next_move_at_sec == 0,
    // and caps recent_centers at 8 across many steps
    with_pve_special_reg(&ctx, |reg| {
        reg.sim_clock_sec = 1_800_000_000;
        reg.roaming_storms.push(RoamingStormRecord {
            storm_id: 1,
            weather: AbyssalWeatherKind::Exotic,
            center_solar_system_id: 30_000_208,
            strong_core_systems: vec![30_000_208],
            weak_periphery_systems: vec![],
            step_count: 0,
            next_move_at_sec: 0,
            recent_centers: vec![],
        });
        advance_roaming_storms(&ctx, reg, 0);
        assert_eq!(reg.roaming_storms[0].recent_centers, vec![30_000_208]);
        assert!(reg.roaming_storms[0].next_move_at_sec > 1_800_000_000);

        // init_roaming_storms is a no-op when non-empty
        init_roaming_storms(&ctx, reg);
        assert_eq!(reg.roaming_storms.len(), 1);

        // Advance far into future so up to 8 steps occur and recent_centers drains to <= 8
        reg.roaming_storms[0].recent_centers = vec![1, 2, 3, 4, 5, 6, 7];
        reg.sim_clock_sec = reg.roaming_storms[0].next_move_at_sec + 8 * STORM_MOVE_MAX_SECS;
        advance_roaming_storms(&ctx, reg, 0);
        assert!(reg.roaming_storms[0].recent_centers.len() <= 8);
    });
}

#[test]
fn storms_sync_removes_stale_beacons_and_anomalies_and_handles_overlap() {
    let sde = Arc::new(make_nullsec_sde_ring());
    let db = Arc::new(WorldRepository::new_empty());
    let ctx = ServerContext::with_sde_and_repository(sde, db);
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_800_000_000)));

    // Seed stale storm beacons in 30_000_211 (no storm) and 30_000_208 (strong Electrical, will keep)
    // and 30_000_209 (strong Electrical, will transition to weak Electrical when storm shrinks!)
    // plus stale DNG-9042 / DNG-9043 anomalies in 30_000_211
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            900_001,
            InventoryItemRecord {
                item_id: 900_001,
                type_id: 47382, // Strong Electrical in 30_000_211 (stale, completely leaving)
                owner_id: 1,
                location_id: 30_000_211,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            900_002,
            InventoryItemRecord {
                item_id: 900_002,
                type_id: 47382, // Strong Electrical in 30_000_209 (will transition to Weak 47381)
                owner_id: 1,
                location_id: 30_000_209,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            900_003,
            InventoryItemRecord {
                item_id: 900_003,
                type_id: 34, // Non-storm item must be untouched
                owner_id: 1,
                location_id: 30_000_211,
                flag: InventoryFlag::Hangar,
                quantity: 10,
                singleton: false,
                ..Default::default()
            },
        );
        snap.cosmic_anomalies.insert(
            30_000_211,
            vec![
                CosmicAnomalyRecord {
                    anomaly_id: "DNG-9042-30000211".into(),
                    name: "Stale Ice".into(),
                    solar_system_id: 30_000_211,
                    anomaly_type: "OreAnomaly".into(),
                    position: [0.0; 3],
                    signal_strength: 1.0,
                    ore_types: vec![],
                    remaining_volume: 0.0,
                    created_unix: 0,
                    lifetime_seconds: 3600,
                    cleared: false,
                },
                CosmicAnomalyRecord {
                    anomaly_id: "DNG-9043-30000211".into(),
                    name: "Stale Surge".into(),
                    solar_system_id: 30_000_211,
                    anomaly_type: "CombatAnomaly".into(),
                    position: [0.0; 3],
                    signal_strength: 1.0,
                    ore_types: vec![],
                    remaining_volume: 0.0,
                    created_unix: 0,
                    lifetime_seconds: 3600,
                    cleared: false,
                },
                CosmicAnomalyRecord {
                    anomaly_id: "ANOM-KEEP-1".into(),
                    name: "Regular Site".into(),
                    solar_system_id: 30_000_211,
                    anomaly_type: "CombatAnomaly".into(),
                    position: [0.0; 3],
                    signal_strength: 1.0,
                    ore_types: vec![],
                    remaining_volume: 0.0,
                    created_unix: 0,
                    lifetime_seconds: 3600,
                    cleared: false,
                },
            ],
        );
    });

    // Two overlapping storms: Storm 1 core=[208], periph=[208, 209] (208 in core skips periph!);
    // Storm 2 also puts 208 in core to test dedup of strong_tid
    with_pve_special_reg(&ctx, |reg| {
        reg.roaming_storms = vec![
            RoamingStormRecord {
                storm_id: 1,
                weather: AbyssalWeatherKind::Electrical,
                center_solar_system_id: 30_000_208,
                strong_core_systems: vec![30_000_208],
                weak_periphery_systems: vec![30_000_208, 30_000_209],
                step_count: 1,
                next_move_at_sec: 1_900_000_000,
                recent_centers: vec![30_000_208],
            },
            RoamingStormRecord {
                storm_id: 2,
                weather: AbyssalWeatherKind::Electrical,
                center_solar_system_id: 30_000_208,
                strong_core_systems: vec![30_000_208],
                weak_periphery_systems: vec![30_000_209],
                step_count: 1,
                next_move_at_sec: 1_900_000_000,
                recent_centers: vec![30_000_208],
            },
        ];
    });

    sync_metaliminal_storm_effects_and_sites(&ctx);

    ctx.db.with_snapshot(|snap| {
        // Stale beacons removed, non-storm item kept
        assert!(!snap.items.contains_key(&900_001));
        assert!(!snap.items.contains_key(&900_002));
        assert!(snap.items.contains_key(&900_003));
        // 30_000_208 has strong Electrical (47382), 30_000_209 has weak Electrical (47381)
        assert!(snap.items.values().any(|i| i.location_id == 30_000_208 && i.type_id == 47382));
        assert!(snap.items.values().any(|i| i.location_id == 30_000_209 && i.type_id == 47381));
        // Stale DNG-9042 / DNG-9043 removed from 30_000_211 while ANOM-KEEP-1 kept
        let anoms_211 = &snap.cosmic_anomalies[&30_000_211];
        assert_eq!(anoms_211.len(), 1);
        assert_eq!(anoms_211[0].anomaly_id, "ANOM-KEEP-1");
        // Core 30_000_208 spawned DNG-9042 and DNG-9043
        assert_eq!(snap.cosmic_anomalies[&30_000_208].len(), 2);
    });

    // Running sync a second time keeps existing beacons and anomalies without duplicating
    sync_metaliminal_storm_effects_and_sites(&ctx);
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.cosmic_anomalies[&30_000_208].len(), 2);
    });
}
