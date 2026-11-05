//! Unit tests for 3-tier LOD living-world director, faction resolution, interdiction, and NPC mining (`living_world`).

use super::*;
use crate::system_actor::SystemRegistry;
use eve_db::clock::VirtualServerClock;
use eve_db::WorldRepository;
use eve_destiny::MovementMode;
use eve_sde::SdeRegistry;
use glam::DVec3;
use std::sync::Arc;

fn build_living_world_fixture() -> (Arc<SdeRegistry>, Arc<WorldRepository>, Arc<SystemRegistry>) {
    let mut sde = SdeRegistry::with_canonical_seed();
    let template_sys = sde
        .solar_systems
        .get(&30_000_142)
        .expect("Jita present in canonical SDE")
        .clone();
    let template_const = sde
        .constellations
        .get(&template_sys.constellation_id)
        .expect("Jita constellation present")
        .clone();

    // Add synthetic empire constellations + systems for Minmatar (500_002), Amarr (500_003),
    // Gallente (500_004), plus an unowned nullsec system (faction_id = None).
    let synthetic = [
        (30_900_002u32, 20_900_002u32, 60_900_002i64, Some(500_002u32)),
        (30_900_003u32, 20_900_003u32, 60_900_003i64, Some(500_003u32)),
        (30_900_004u32, 20_900_004u32, 60_900_004i64, Some(500_004u32)),
        (30_900_099u32, 20_900_099u32, 60_900_099i64, None),
    ];
    for (sys_id, const_id, st_id, faction_id) in synthetic {
        let mut c = template_const.clone();
        c.constellation_id = const_id;
        c.faction_id = faction_id;
        if faction_id.is_none() {
            c.region_id = 10_900_099;
        }
        sde.constellations.insert(const_id, c);

        let mut sys = template_sys.clone();
        sys.solar_system_id = sys_id;
        sys.constellation_id = const_id;
        if faction_id.is_none() {
            sys.region_id = 10_900_099;
        }
        if let Some(st) = sys.stations.first_mut() {
            st.station_id = st_id;
            st.solar_system_id = sys_id;
            st.constellation_id = const_id;
            st.region_id = sys.region_id;
            st.owner_faction_id = faction_id.map(|f| f as i64);
            st.owner_corporation_id = 0;
            sde.stations.insert(st_id, st.clone());
        }
        sde.solar_systems.insert(sys_id, sys);
    }

    // Clamp Venture (32880) Mining Hold (attr 1556) & capacity + Miner I (483) attributes.
    if let Some(t) = sde.types.get_mut(&32880) {
        t.capacity = 500.0; // 5,000 units of Veldspar (0.1 m3)
        t.default_attributes.insert(1556, 500.0);
        t.default_attributes.insert(102, 2.0);
        t.default_attributes.insert(3239, 100.0);
    }
    if let Some(t) = sde.types.get_mut(&483) {
        t.default_attributes.insert(77, 250.0);
        t.default_attributes.insert(73, 5_000.0);
    }

    let sde = Arc::new(sde);
    let db = Arc::new(WorldRepository::new_empty());
    db.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_760_000_000)));
    let _ = crate::world_seeder::seed_dual_anchor_markets(&db, &sde);
    let systems = Arc::new(SystemRegistry::with_sde_and_db(sde.clone(), db.clone()));
    (sde, db, systems)
}

#[test]
fn sde_ball_combat_and_faction_hull_resolution() {
    let (sde, _, _) = build_living_world_fixture();

    let (known_ball, known_combat) =
        sde_ball_and_combat(&sde, 42, 648, DVec3::new(100.0, 200.0, 300.0));
    assert_eq!(known_ball.id, 42);
    assert_eq!(known_ball.type_id, 648);
    assert_eq!(known_ball.mode, MovementMode::Stop);
    assert!(known_ball.mass >= 1_000.0);
    assert!(known_ball.max_velocity >= 1.0);
    assert!(known_combat.max_shield_hp >= 1.0);

    // Unknown type_id exercises fallback ball & combat parameters.
    let (fallback_ball, fallback_combat) =
        sde_ball_and_combat(&sde, 99, 999_999_999, DVec3::ZERO);
    assert_eq!(fallback_ball.mass, 1_200_000.0);
    assert_eq!(fallback_ball.radius, 40.0);
    assert_eq!(fallback_ball.max_velocity, 250.0);
    assert_eq!(fallback_combat.max_shield_hp, 1_500.0);

    // Empire faction resolution for hauler and miner hulls.
    assert_eq!(hauler_type_for_system(&sde, 30_000_142), 648); // Caldari Badger
    assert_eq!(hauler_type_for_system(&sde, 30_900_002), 653); // Minmatar Wreathe
    assert_eq!(hauler_type_for_system(&sde, 30_900_003), 649); // Amarr Bestower
    assert_eq!(hauler_type_for_system(&sde, 30_900_004), 651); // Gallente Iteron V
    assert_eq!(hauler_type_for_system(&sde, 99_999_999), 648); // Unknown system fallback

    assert_eq!(miner_type_for_system(&sde, 30_000_142), 21654);
    assert_eq!(miner_type_for_system(&sde, 30_900_002), 20163);
    assert_eq!(miner_type_for_system(&sde, 30_900_003), 26750);
    assert_eq!(miner_type_for_system(&sde, 30_900_004), 21840);
}

#[tokio::test]
async fn director_registry_materialize_lod2_and_interdiction() {
    let (sde, db, systems) = build_living_world_fixture();

    let empty_dir = LivingWorldDirector::default();
    assert!(empty_dir.list_convoys().is_empty());

    let director = LivingWorldDirector::new_seeded();
    let seeded = director.list_convoys();
    assert_eq!(seeded.len(), 3);
    assert_eq!(seeded[0].convoy_id, 800_000_001);
    assert_eq!(seeded[1].convoy_id, 800_000_002);
    assert_eq!(seeded[2].convoy_id, 800_000_003);

    // Companion mining hauler lookup checks ID threshold and archetype.
    assert!(director.get_mining_hauler_convoy(500_000).is_none());
    assert!(director
        .get_mining_hauler_convoy(1_000_000 + 800_000_001)
        .is_none());
    let mining_parent = director
        .get_mining_hauler_convoy(1_000_000 + 800_000_002)
        .expect("MiningOp companion hauler resolves parent");
    assert_eq!(mining_parent.convoy_id, 800_000_002);

    // Interdicting missing or non-LOD2 convoy returns error.
    assert!(director
        .interdict_convoy(999_999_999, 2112000001, &db)
        .is_err());
    let err_lod1 = director
        .interdict_convoy(800_000_001, 2112000001, &db)
        .unwrap_err();
    assert!(err_lod1.contains("must be materialized at LOD 2"));

    // Materialize Perimeter (MiningOp + companion hauler) and verify idempotency.
    let spawned_perim = director
        .materialize_system_lod2(30_000_144, &systems, &sde)
        .await;
    assert_eq!(spawned_perim, 1);
    assert_eq!(
        director
            .materialize_system_lod2(30_000_144, &systems, &sde)
            .await,
        0
    );
    assert!(systems
        .get_ball_position_sync(30_000_144, 800_000_002)
        .is_some());
    assert!(systems
        .get_ball_position_sync(30_000_144, 1_000_000 + 800_000_002)
        .is_some());

    // Materialize Jita HaulerConvoy to LOD 2 and interdict it.
    let spawned_jita = director
        .materialize_system_lod2(30_000_142, &systems, &sde)
        .await;
    assert_eq!(spawned_jita, 1);

    let (wreck_id, loot_id, ask_price) = director
        .interdict_convoy(800_000_001, 2112000001, &db)
        .expect("LOD 2 convoy interdiction succeeds");
    assert!(ask_price.0 > 0);
    db.with_snapshot(|snap| {
        let wreck = snap.items.get(&wreck_id).expect("wreck created in db");
        assert_eq!(wreck.type_id, 26557);
        assert_eq!(wreck.owner_id, 2112000001);
        let loot = snap.items.get(&loot_id).expect("cargo loot created in db");
        assert_eq!(loot.location_id, wreck_id);
        assert_eq!(loot.type_id, 34);
        assert_eq!(loot.quantity, 50_000);
    });

    // Destroyed convoy cannot be interdicted a second time.
    assert!(director
        .interdict_convoy(800_000_001, 2112000001, &db)
        .is_err());

    // Combat destruction & hauler cooldown enforcement.
    let hc_id = director.ensure_system_hauler_convoy(30_000_144, &sde);
    director.mark_convoy_destroyed_in_combat(hc_id);
    assert!(director.get_convoy(hc_id).unwrap().destroyed);
    assert_eq!(director.ensure_system_hauler_convoy(30_000_144, &sde), hc_id);
    // Once cooldown expires (or is cleared), ensure_system_hauler_convoy spawns a fresh hauler.
    director.hauler_cooldowns.write().unwrap().clear();
    let respawned_hc = director.ensure_system_hauler_convoy(30_000_144, &sde);
    assert!(!director.get_convoy(respawned_hc).unwrap().destroyed);
}

#[test]
fn director_ensure_system_entities_and_transit_duration() {
    let (sde, _, _) = build_living_world_fixture();
    let director = LivingWorldDirector::new();

    // Ensure MiningOp & idempotency.
    let m1 = director.ensure_system_mining_op(30_900_002, &sde);
    assert_eq!(m1, 800_100_000 + 30_900_002 % 100_000);
    assert_eq!(director.ensure_system_mining_op(30_900_002, &sde), m1);

    // Ensure EmpireNavyPatrol for Caldari/Minmatar/Amarr/Gallente + non-empire/missing systems.
    let p_cal = director.ensure_system_navy_patrol(30_000_142, &sde).unwrap();
    let p_min = director.ensure_system_navy_patrol(30_900_002, &sde).unwrap();
    let p_ama = director.ensure_system_navy_patrol(30_900_003, &sde).unwrap();
    let p_gal = director.ensure_system_navy_patrol(30_900_004, &sde).unwrap();
    assert_eq!(director.get_convoy(p_cal).unwrap().ship_type_id, 52_936);
    assert_eq!(director.get_convoy(p_min).unwrap().ship_type_id, 52_959);
    assert_eq!(director.get_convoy(p_ama).unwrap().ship_type_id, 52_927);
    assert_eq!(director.get_convoy(p_gal).unwrap().ship_type_id, 52_950);
    assert!(director.ensure_system_navy_patrol(30_900_099, &sde).is_none());
    assert!(director.ensure_system_navy_patrol(99_999_999, &sde).is_none());

    // Hauler transit duration with known gates/stations and fallback 15 AU.
    let d_known = LivingWorldDirector::compute_hauler_transit_duration_secs(
        &sde,
        648,
        30_000_142,
        Some(30_000_144),
        Some(30_000_144),
        60_003_760,
    );
    assert!(d_known >= 7);

    let d_fallback = LivingWorldDirector::compute_hauler_transit_duration_secs(
        &sde,
        999_999_999,
        99_999_999,
        None,
        None,
        0,
    );
    assert!(d_fallback > 10);
}

#[tokio::test]
async fn npc_mining_op_full_cycle_haul_and_destroyed_hauler_recovery() {
    let (sde, db, systems) = build_living_world_fixture();
    let director = LivingWorldDirector::new();

    // Error paths: missing convoy, wrong archetype, destroyed convoy.
    assert!(director
        .step_npc_mining_op(999_999_999, &systems, &db, &sde)
        .await
        .is_err());

    let wrong_arch = LivingWorldConvoy {
        convoy_id: 800_999_001,
        archetype: NpcArchetype::HaulerConvoy,
        ship_type_id: 648,
        origin_system_id: 30_000_144,
        current_system_id: 30_000_144,
        destination_system_id: 30_000_142,
        destination_station_id: 60_003_760,
        cargo_type_id: 34,
        cargo_quantity: 0,
        lod_level: 1,
        destroyed: false,
    };
    director.register_convoy(wrong_arch);
    assert!(director
        .step_npc_mining_op(800_999_001, &systems, &db, &sde)
        .await
        .is_err());

    let destroyed_op = LivingWorldConvoy {
        convoy_id: 800_999_002,
        archetype: NpcArchetype::MiningOp,
        ship_type_id: 32880,
        origin_system_id: 30_000_142,
        current_system_id: 30_000_142,
        destination_system_id: 30_000_142,
        destination_station_id: 60_003_760,
        cargo_type_id: 1230,
        cargo_quantity: 0,
        lod_level: 1,
        destroyed: true,
    };
    director.register_convoy(destroyed_op);
    assert!(director
        .step_npc_mining_op(800_999_002, &systems, &db, &sde)
        .await
        .is_err());

    // Synthetic system 30_900_002 has no asteroid belts -> returns Err("No minable asteroids...").
    let no_belt_op_id = 800_999_004;
    director.register_convoy(LivingWorldConvoy {
        convoy_id: no_belt_op_id,
        archetype: NpcArchetype::MiningOp,
        ship_type_id: 32880,
        origin_system_id: 30_900_002,
        current_system_id: 30_900_002,
        destination_system_id: 30_900_002,
        destination_station_id: 60_900_002,
        cargo_type_id: 1230,
        cargo_quantity: 0,
        lod_level: 1,
        destroyed: false,
    });
    assert!(director
        .step_npc_mining_op(no_belt_op_id, &systems, &db, &sde)
        .await
        .is_err());
    director.mark_convoy_destroyed(no_belt_op_id);

    // Primary MiningOp in Perimeter (30_000_144): initial step spawns miner & hauler at belt.
    let mining_id = director.ensure_system_mining_op(30_000_144, &sde);
    {
        let mut guard = director.convoys.write().unwrap();
        let c = guard.get_mut(&mining_id).unwrap();
        c.ship_type_id = 32880; // Venture (500 m3 hold)
    }

    // Destroyed-hauler-in-transit branch: hauler has cargo in db without ball on grid -> clears cargo.
    let lost_hauler_op_id = 800_999_003;
    director.register_convoy(LivingWorldConvoy {
        convoy_id: lost_hauler_op_id,
        archetype: NpcArchetype::MiningOp,
        ship_type_id: 32880,
        origin_system_id: 30_000_144,
        current_system_id: 30_000_144,
        destination_system_id: 30_000_144,
        destination_station_id: 60_003_766,
        cargo_type_id: 1230,
        cargo_quantity: 0,
        lod_level: 2,
        destroyed: false,
    });
    db.with_mut_snapshot(|snap| {
        snap.next_entity_id += 1;
        let item_id = snap.next_entity_id;
        snap.items.insert(
            item_id,
            InventoryItemRecord {
                item_id,
                type_id: 1230,
                owner_id: 1000125,
                location_id: lost_hauler_op_id + 1_000_000,
                flag: InventoryFlag::Cargo,
                quantity: 1_200,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });
    assert_eq!(director.hauler_cargo_units(&db, lost_hauler_op_id), 1_200);
    let lost_step = director
        .step_npc_mining_op(lost_hauler_op_id, &systems, &db, &sde)
        .await
        .expect("mining op recovers when companion hauler disappears");
    assert!(!lost_step.hauled_to_station);
    assert_eq!(director.hauler_cargo_units(&db, lost_hauler_op_id), 0);

    // Mark lost_hauler_op_id destroyed so subsequent batch focuses on primary op.
    director.mark_convoy_destroyed(lost_hauler_op_id);
    let step1 = director
        .step_npc_mining_op(mining_id, &systems, &db, &sde)
        .await
        .expect("step 1 mines initial units");
    assert_eq!(step1.convoy_id, mining_id);
    assert!(!step1.hauled_to_station);
    assert_eq!(director.hauler_cargo_units(&db, mining_id), 0);

    // Advance clock +300s -> Venture fills hold (500 m3 / 0.1 = 5,000 units), loads companion hauler.
    db.clock().advance_secs(300);
    let step2 = director
        .step_npc_mining_op(mining_id, &systems, &db, &sde)
        .await
        .expect("step 2 fills ore hold and loads companion hauler");
    assert_eq!(step2.convoy_cargo_units, 0);
    assert!(!step2.hauled_to_station);
    let loaded = director.hauler_cargo_units(&db, mining_id);
    assert!(loaded >= 5_000, "loaded cargo was {loaded}");

    // Advance clock +30s (elapsed >= 15.0s -> arrived at station), step_all_mining_ops delivers ore.
    db.clock().advance_secs(30);
    let batch = director.step_all_mining_ops(&systems, &db, &sde).await;
    assert_eq!(batch.len(), 1);
    let (_, res) = &batch[0];
    let step3 = res.as_ref().expect("step 3 completes haul delivery");
    assert!(step3.hauled_to_station);
    assert_eq!(director.hauler_cargo_units(&db, mining_id), 0);
}
