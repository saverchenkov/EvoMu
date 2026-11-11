//! Comprehensive edge-case unit tests for `pve_exploration_sites.rs`.

#![forbid(unsafe_code)]

use super::*;
use crate::router::ServerContext;
use crate::services::combat_crimewatch::CombatRuntimeState;
use crate::services::pve_exploration::SpewedLootCan;
use eve_db::clock::VirtualServerClock;
use eve_db::WorldRepository;
use eve_sde::{ConstellationInfo, InvType, RegionInfo, SdeRegistry, SolarSystem};
use glam::DVec3;
use std::sync::{Arc, OnceLock, RwLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

fn synthetic_sde_for_factions_and_sleepers() -> SdeRegistry {
    let mut sde = SdeRegistry::default();
    let base_sys = shared_sde().solar_systems.values().next().cloned().unwrap();
    let base_type = shared_sde().types.values().next().cloned().unwrap();

    // 5 regions & systems mapping to each PirateFaction arm in pirate_faction_for_system:
    // 500010 -> Guristas (highsec), 500012 -> BloodRaiders (lowsec),
    // 500012 + "Stain" -> Sansha (nullsec <= 0.0), 500011 -> Serpentis (highsec), 500015 -> AngelCartel (nullsec)
    let defs = [
        (9101u32, 9201u32, 9301u32, "Lonetrek", 500010u32, 0.8f32),
        (9102, 9202, 9302, "Bleak Lands", 500012, 0.3f32),
        (9103, 9203, 9303, "Stain", 500012, -0.5f32),
        (9104, 9204, 9304, "Sinq Laison", 500011, 0.6f32),
        (9105, 9205, 9305, "Curse", 500015, -0.3f32),
    ];
    for (rid, cid, sid, rname, fac, sec) in defs {
        sde.regions.insert(
            rid,
            RegionInfo {
                region_id: rid,
                name: rname.into(),
                center: [0.0; 3],
                faction_id: Some(fac),
                wormhole_class_id: None,
            },
        );
        sde.constellations.insert(
            cid,
            ConstellationInfo {
                constellation_id: cid,
                region_id: rid,
                name: format!("Con-{cid}"),
                center: [0.0; 3],
                faction_id: Some(fac),
                wormhole_class_id: None,
            },
        );
        let mut sys: SolarSystem = base_sys.clone();
        sys.solar_system_id = sid;
        sys.region_id = rid;
        sys.constellation_id = cid;
        sys.security_status = sec as f64;
        sde.solar_systems.insert(sid, sys);
    }

    // Loot types across all relevant groups
    let type_specs = [
        (3331u32, DATACORE_GROUP_ID, "Caldari Datacore - Graviton Physics"),
        (3332, DATACORE_GROUP_ID, "Amarr Datacore - Laser Physics"),
        (3333, DATACORE_GROUP_ID, "Gallente Datacore - Plasma Physics"),
        (3334, DATACORE_GROUP_ID, "Minmatar Datacore - Rocket Science"),
        (7281, DECRYPTOR_GROUP_IDS[0], "Accelerant Decryptor"),
        (7282, DECRYPTOR_GROUP_IDS[8], "Parity Decryptor"),
        (7541, SALVAGED_MATERIALS_GROUP_ID, "Logic Circuit"),
        (7542, SALVAGED_MATERIALS_GROUP_ID, "Alloyed Tritanium Bar"),
        (7543, SALVAGED_MATERIALS_GROUP_ID, "Armor Plates"),
        (7544, SALVAGED_MATERIALS_GROUP_ID, "Tripped Power Circuit"),
        (7545, SALVAGED_MATERIALS_GROUP_ID, "Burned Logic Circuit"),
        (9661, ANCIENT_SALVAGE_GROUP_ID, "Talocan Electronic Library"),
        (9662, ANCIENT_SALVAGE_GROUP_ID, "Sleeper Nanoribbons"),
        // DED gate test hull types: Frigate (25), Cruiser (26), Battleship (27), Capital (485)
        (587, 25, "Rifter"),
        (622, 26, "Stabber"),
        (638, 27, "Raven"),
        (19720, 485, "Revelation"),
    ];
    for (tid, gid, name) in type_specs {
        let mut t: InvType = base_type.clone();
        t.type_id = tid;
        t.group_id = gid;
        t.name = name.into();
        t.published = true;
        sde.types.insert(tid, t);
    }
    sde
}

#[test]
fn all_five_pirate_factions_sec_bands_and_ded_gates_covered() {
    let syn = synthetic_sde_for_factions_and_sleepers();

    // All 5 factions + unknown solar system (defaults true_sec = 0.5, Guristas)
    for sid in [9301u32, 9302, 9303, 9304, 9305, 999_999] {
        let data = generate_exploration_container_loot(&syn, sid, false);
        let relic = generate_exploration_container_loot(&syn, sid, true);
        assert!(!data.is_empty(), "data loot empty for system {sid}");
        assert!(!relic.is_empty(), "relic loot empty for system {sid}");
        if sid == 9303 || sid == 9305 {
            // Null-sec deep_space relic includes Ancient Salvage (group 966)
            assert!(relic.iter().any(|(tid, _, _)| *tid == 9661 || *tid == 9662));
            // Quantity scaled by sec_mult = 4
            assert!(data.iter().any(|(_, qty, _)| *qty == 12));
        } else if sid == 9302 {
            // Low-sec scaled by sec_mult = 2
            assert!(data.iter().any(|(_, qty, _)| *qty == 6));
        }
    }

    // DED gate ratings 1..=3, 4..=6, 7..=10, plus 0 / 11 fallback
    assert_eq!(ded_ship_restriction_for_rating(0).0, 250_000_000.0);
    assert_eq!(ded_ship_restriction_for_rating(1).0, 10_000_000.0);
    assert_eq!(ded_ship_restriction_for_rating(4).0, 75_000_000.0);
    assert_eq!(ded_ship_restriction_for_rating(10).0, 250_000_000.0);

    // Exact mass boundary passes; +1 kg fails
    assert!(check_ded_gate_ship_allowed(&syn, 3, 587, 10_000_000.0).is_ok());
    assert_eq!(
        check_ded_gate_ship_allowed(&syn, 3, 587, 10_000_001.0).unwrap_err().code,
        "ShipSizeRestricted"
    );
    // Cruiser disallowed in 3/10, allowed in 4/10
    assert!(check_ded_gate_ship_allowed(&syn, 3, 622, 10_000_000.0).is_err());
    assert!(check_ded_gate_ship_allowed(&syn, 4, 622, 15_000_000.0).is_ok());
    // Battleship disallowed in 6/10, allowed in 7/10; Capital disallowed even in 10/10
    assert!(check_ded_gate_ship_allowed(&syn, 6, 638, 100_000_000.0).is_err());
    assert!(check_ded_gate_ship_allowed(&syn, 7, 638, 250_000_000.0).is_ok());
    assert!(check_ded_gate_ship_allowed(&syn, 10, 19720, 200_000_000.0).is_err());

    // Empty SDE edge cases for overseer_effects_for_rating and sleeper_blue_loot_for_class
    let empty_sde = SdeRegistry::default();
    assert_eq!(overseer_effects_for_rating(&empty_sde, 5), None);
    assert_eq!(sleeper_blue_loot_for_class(&empty_sde, 3), None);

    // Canonical SDE overseer effects clamping for 0 and 20
    let full_sde = shared_sde();
    assert_eq!(overseer_effects_for_rating(&full_sde, 0), overseer_effects_for_rating(&full_sde, 1));
    assert_eq!(overseer_effects_for_rating(&full_sde, 20), overseer_effects_for_rating(&full_sde, 10));
    assert!(sleeper_blue_loot_for_class(&full_sde, 0).is_some());
    assert!(sleeper_blue_loot_for_class(&full_sde, 9).is_some());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ded_wreck_bands_sleeper_rep_attributes_and_empty_sde_fallbacks() {
    let empty_sde = Arc::new(SdeRegistry::default());
    let db = Arc::new(WorldRepository::new_empty());
    db.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_776_200_000)));
    let ctx_empty = ServerContext::with_sde_and_repository(empty_sde.clone(), db.clone());
    let sys_id: SolarSystemId = 31_000_010;
    let char_id: i64 = 90_100_099;

    // With empty SDE: spawn_ded_overseer_wreck_core skips overseer effects & deadspace mod, still spawns wreck
    let (wid_empty, loot_empty) = spawn_ded_overseer_wreck_core(
        &ctx_empty.db,
        &ctx_empty.systems,
        &ctx_empty.sde,
        30_000_142,
        char_id,
        DVec3::ZERO,
        5,
    )
    .await;
    assert!(wid_empty > 0);
    assert!(loot_empty.is_empty());

    // With empty SDE: drop_sleeper_wreck_core skips cargo loot when sleeper_blue_loot_for_class is None
    let sw_empty = drop_sleeper_wreck_core(
        &ctx_empty.db,
        &ctx_empty.systems,
        &ctx_empty.sde,
        sys_id,
        char_id,
        DVec3::new(100.0, 0.0, 0.0),
        2,
    )
    .await;
    assert!(sw_empty > 0);
    ctx_empty.db.with_snapshot(|snap| {
        assert_eq!(snap.items[&sw_empty].type_id, 30495);
        assert!(!snap.items.values().any(|i| i.location_id == sw_empty));
    });

    // Full SDE: DED 4..=6 band picks 18672 (Gistum A-Type 10MN) and Battleship wreck 26561
    let full_sde = shared_sde();
    let (wid_mid, loot_mid) = spawn_ded_overseer_wreck_core(
        &ctx_empty.db,
        &ctx_empty.systems,
        &full_sde,
        30_000_142,
        char_id,
        DVec3::new(200.0, 0.0, 0.0),
        5,
    )
    .await;
    assert_eq!(ctx_empty.db.with_snapshot(|s| s.items[&wid_mid].type_id), 26561);
    assert!(loot_mid.iter().any(|(tid, _, _)| *tid == 18672));

    // Custom SDE sleeper with remote_shield_rep (68), remote_armor_rep (84), and negative web_speed_factor (20)
    let mut custom_sde = (*full_sde).clone();
    let base_type = full_sde.types.values().next().cloned().unwrap();
    let mut rep_sleeper = base_type.clone();
    rep_sleeper.type_id = 30215; // C1 wave 1 trigger
    rep_sleeper.default_attributes.insert(68, 250.0);
    rep_sleeper.default_attributes.insert(84, 150.0);
    rep_sleeper.default_attributes.insert(20, -60.0);
    custom_sde.types.insert(30215, rep_sleeper);

    let combat_rt = Arc::new(RwLock::new(CombatRuntimeState::default()));
    let custom_sde_arc = Arc::new(custom_sde);
    let spawned = spawn_sleeper_wave_core(
        &ctx_empty.systems,
        &ctx_empty.db,
        &custom_sde_arc,
        &combat_rt,
        sys_id,
        1,
        1,
        DVec3::ZERO,
    )
    .await;
    assert_eq!(spawned.len(), 4);
    let trig_entity = combat_rt.read().unwrap().npc_entities[&spawned[0]].clone();
    assert_eq!(trig_entity.remote_shield_rep, 250.0);
    assert_eq!(trig_entity.remote_armor_rep, 150.0);
    assert_eq!(trig_entity.remote_rep_range, 25_000.0);
    assert_eq!(trig_entity.web_speed_factor, -60.0);

    // Multi-can spew across two systems
    let cans = vec![
        SpewedLootCan {
            can_item_id: 2_900_001,
            parent_container_id: 2_800_001,
            solar_system_id: 30_000_142,
            position: [10.0, 20.0, 30.0],
            loot_type_id: 34,
            loot_quantity: 50,
            spawned_at_sec: 1_776_200_000,
            despawn_at_sec: 1_776_203_600,
            scooped: false,
        },
        SpewedLootCan {
            can_item_id: 2_900_002,
            parent_container_id: 2_800_001,
            solar_system_id: sys_id,
            position: [40.0, 50.0, 60.0],
            loot_type_id: 35,
            loot_quantity: 75,
            spawned_at_sec: 1_776_200_000,
            despawn_at_sec: 1_776_203_600,
            scooped: false,
        },
    ];
    materialize_spewed_cans_core(&ctx_empty.db, &ctx_empty.systems, char_id, &cans).await;
    assert!(ctx_empty.systems.get_or_spawn(30_000_142).get_ball_position_sync(2_900_001).is_some());
    assert!(ctx_empty.systems.get_or_spawn(sys_id).get_ball_position_sync(2_900_002).is_some());
}
