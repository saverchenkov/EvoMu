//! Additional edge-case and branch-coverage tests for `living_world`.

#![forbid(unsafe_code)]

use super::*;
use crate::system_actor::SystemRegistry;
use eve_db::clock::VirtualServerClock;
use eve_db::{InventoryFlag, InventoryItemRecord, IskCents, MarketOrderRecord, ServerClock, WorldRepository};
use eve_destiny::MovementMode;
use eve_sde::{SdeRegistry, SolarSystem};
use glam::DVec3;
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

#[test]
fn living_world_default_unknown_types_fallback_tiers_and_interdict_errors() {
    let mut sde = (*shared_sde()).clone();

    // 1. LivingWorldDirector::default() starts empty.
    let dir = LivingWorldDirector::default();
    assert!(dir.list_convoys().is_empty());

    // 2. sde_ball_and_combat for unknown type_id uses outer unwrap_or (shield_hp == 1_500.0, mass == 1_200_000.0).
    let (ball, combat) = sde_ball_and_combat(&sde, 42, 999_888_777, DVec3::new(1.0, 2.0, 3.0));
    assert_eq!(ball.mass, 1_200_000.0);
    assert_eq!(ball.radius, 40.0);
    assert_eq!(ball.max_velocity, 250.0);
    assert_eq!(ball.inertia, 3.5);
    assert_eq!(combat.shield_hp, 1_500.0);
    assert_eq!(combat.armor_hp, 1_200.0);
    assert_eq!(combat.structure_hp, 1_500.0);

    // 3. system_faction_id when station has owner_faction_id=None and unknown corp, no constellation/region faction -> 500_001.
    let jita = sde.solar_systems[&30_000_142].clone();
    let mut reg = sde.regions[&jita.region_id].clone();
    reg.region_id = 10_950_001;
    reg.faction_id = None;
    sde.regions.insert(10_950_001, reg);
    let mut con = sde.constellations[&jita.constellation_id].clone();
    con.constellation_id = 20_950_001;
    con.region_id = 10_950_001;
    con.faction_id = None;
    sde.constellations.insert(20_950_001, con);
    let mut st = jita.stations[0].clone();
    st.station_id = 60_950_001;
    st.solar_system_id = 30_950_001;
    st.owner_faction_id = None;
    st.owner_corporation_id = 999_999_999; // absent from npc_corporations
    let mut sys: SolarSystem = jita.clone();
    sys.solar_system_id = 30_950_001;
    sys.constellation_id = 20_950_001;
    sys.region_id = 10_950_001;
    sys.stations = vec![st];
    sde.solar_systems.insert(30_950_001, sys);
    assert_eq!(hauler_type_for_system(&sde, 30_950_001), 648);
    assert_eq!(miner_type_for_system(&sde, 30_950_001), 21654);

    // 4. ensure_system_navy_patrol: unknown system -> None; Some(0) faction_id ignored; empty stations -> dest_station_id == 0.
    assert!(dir.ensure_system_navy_patrol(99_999_999, &sde).is_none());
    let mut sys_zero_fac = jita;
    sys_zero_fac.solar_system_id = 30_950_002;
    sys_zero_fac.constellation_id = 20_950_001;
    sys_zero_fac.region_id = 10_950_001;
    sys_zero_fac.stations.clear();
    sde.solar_systems.insert(30_950_002, sys_zero_fac);
    // Region faction_id = Some(0) is filtered out by filter(|&f| f > 0) -> None.
    sde.regions.get_mut(&10_950_001).unwrap().faction_id = Some(0);
    assert!(dir.ensure_system_navy_patrol(30_950_002, &sde).is_none());
    // Region faction_id = Some(500_003) with empty stations -> destination_station_id == 0.
    sde.regions.get_mut(&10_950_001).unwrap().faction_id = Some(500_003);
    let navy_id = dir.ensure_system_navy_patrol(30_950_002, &sde).unwrap();
    assert_eq!(dir.get_convoy(navy_id).unwrap().destination_station_id, 0);

    // 5. get_mining_hauler_convoy rejects non-MiningOp even when hauler_ball_id > 1_000_000.
    dir.register_convoy(LivingWorldConvoy {
        convoy_id: 500,
        archetype: NpcArchetype::HaulerConvoy,
        ship_type_id: 648,
        origin_system_id: 30_000_142,
        current_system_id: 30_000_142,
        destination_system_id: 30_000_144,
        destination_station_id: 60_003_766,
        cargo_type_id: 34,
        cargo_quantity: 1_000,
        lod_level: 1,
        destroyed: false,
    });
    assert!(dir.get_mining_hauler_convoy(1_000_500).is_none());

    // 6. mark_convoy_destroyed and mark_convoy_destroyed_in_combat on missing ID are safe no-ops;
    // mark_convoy_destroyed_in_combat on MiningOp does not insert hauler cooldown.
    dir.mark_convoy_destroyed(999_999);
    dir.mark_convoy_destroyed_in_combat(999_999);
    dir.register_convoy(LivingWorldConvoy {
        convoy_id: 501,
        archetype: NpcArchetype::MiningOp,
        ship_type_id: 32880,
        origin_system_id: 30_000_142,
        current_system_id: 30_000_142,
        destination_system_id: 30_000_142,
        destination_station_id: 60_003_760,
        cargo_type_id: 1230,
        cargo_quantity: 0,
        lod_level: 1,
        destroyed: false,
    });
    dir.mark_convoy_destroyed_in_combat(501);
    assert!(dir.hauler_cooldowns.read().unwrap().is_empty());

    // 7. interdict_convoy error paths: not found, LOD < 2, already destroyed; plus non-matching order filters.
    let db = WorldRepository::new_empty();
    assert!(dir
        .interdict_convoy(999_999, 90_000_001, &db)
        .unwrap_err()
        .contains("not found"));
    assert!(dir
        .interdict_convoy(500, 90_000_001, &db)
        .unwrap_err()
        .contains("LOD 1"));
    assert!(dir
        .interdict_convoy(501, 90_000_001, &db)
        .unwrap_err()
        .contains("already destroyed"));

    // Interdicting LOD 2 convoy ignores NPC ask at wrong station or wrong type_id.
    db.with_mut_snapshot(|snap| {
        snap.market_orders.insert(
            1,
            MarketOrderRecord {
                order_id: 1,
                owner_id: 1_000_035,
                station_id: 60_003_760, // wrong station
                region_id: 10_000_002,
                type_id: 34,
                is_bid: false,
                price: IskCents::from_isk(99),
                vol_entered: 10,
                vol_remaining: 10,
                min_volume: 1,
                is_npc: true,
                escrow: IskCents(0),
            },
        );
        snap.market_orders.insert(
            2,
            MarketOrderRecord {
                order_id: 2,
                owner_id: 1_000_035,
                station_id: 60_003_766,
                region_id: 10_000_002,
                type_id: 35, // wrong type_id
                is_bid: false,
                price: IskCents::from_isk(88),
                vol_entered: 10,
                vol_remaining: 10,
                min_volume: 1,
                is_npc: true,
                escrow: IskCents(0),
            },
        );
    });
    dir.register_convoy(LivingWorldConvoy {
        convoy_id: 502,
        archetype: NpcArchetype::PirateRoamer,
        ship_type_id: 16997,
        origin_system_id: 30_000_142,
        current_system_id: 30_000_142,
        destination_system_id: 30_000_144,
        destination_station_id: 60_003_766,
        cargo_type_id: 34,
        cargo_quantity: 250,
        lod_level: 2,
        destroyed: false,
    });
    let (wreck_id, loot_id, ask) = dir.interdict_convoy(502, 90_000_001, &db).unwrap();
    assert_eq!(ask, IskCents::from_isk(6));
    db.with_snapshot(|snap| {
        assert_eq!(snap.items[&wreck_id].type_id, 26557);
        assert_eq!(snap.items[&loot_id].location_id, wreck_id);
        assert_eq!(snap.items[&loot_id].quantity, 250);
    });

    // 8. ensure_system_mining_op returns existing MiningOp on second call;
    // ensure_system_hauler_convoy returns active non-docked convoy, and recreates after docking or expired cooldown.
    let m1 = dir.ensure_system_mining_op(30_000_144, &sde);
    let m2 = dir.ensure_system_mining_op(30_000_144, &sde);
    assert_eq!(m1, m2);

    let h1 = dir.ensure_system_hauler_convoy(30_000_144, &sde);
    let h2 = dir.ensure_system_hauler_convoy(30_000_144, &sde);
    assert_eq!(h1, h2);
    // Mark route docked -> ensure_system_hauler_convoy recreates fresh non-docked convoy.
    {
        let mut routes = dir.routes.write().unwrap();
        routes.get_mut(&h1).unwrap().docked = true;
    }
    let h3 = dir.ensure_system_hauler_convoy(30_000_144, &sde);
    assert_eq!(h3, h1);
    assert!(!dir.is_convoy_docked(h3));

    // Mark destroyed with an ALREADY EXPIRED cooldown -> recreates with destroyed = false!
    dir.mark_convoy_destroyed(h3);
    {
        let mut cd = dir.hauler_cooldowns.write().unwrap();
        cd.insert(
            30_000_144,
            std::time::Instant::now() - std::time::Duration::from_secs(1),
        );
    }
    let h4 = dir.ensure_system_hauler_convoy(30_000_144, &sde);
    assert_eq!(h4, h3);
    assert!(!dir.get_convoy(h4).unwrap().destroyed);
}

#[tokio::test]
async fn living_world_materialize_filters_mining_stacking_evasion_and_batch_filters() {
    let mut sde_raw = (*shared_sde()).clone();
    // Create a system without asteroid belts (group 9) to cover MiningOp station fallback in materialize_system_lod2.
    let no_belt_sys = 30_955_001_u32;
    let mut sys = sde_raw.solar_systems[&30_000_142].clone();
    sys.solar_system_id = no_belt_sys;
    if let Some(st) = sys.stations.first_mut() {
        st.station_id = 60_955_001;
        st.solar_system_id = no_belt_sys;
        st.position = DVec3::new(10_000.0, 0.0, 0.0);
        sde_raw.stations.insert(60_955_001, st.clone());
    }
    sde_raw.solar_systems.insert(no_belt_sys, sys);
    if let Some(mut peri_st) = sde_raw.solar_systems[&30_000_142].stations.first().cloned() {
        peri_st.station_id = 60_003_766;
        peri_st.solar_system_id = 30_000_144;
        peri_st.position = DVec3::new(100_000_000.0, 0.0, 0.0);
        sde_raw.stations.insert(60_003_766, peri_st.clone());
        sde_raw
            .solar_systems
            .get_mut(&30_000_144)
            .unwrap()
            .stations
            .push(peri_st);
    }
    let sde = Arc::new(sde_raw);

    let clock = Arc::new(VirtualServerClock::with_fixed_base(1_850_000_000));
    let db = Arc::new(WorldRepository::new_empty());
    db.set_clock(clock.clone());
    let systems = Arc::new(SystemRegistry::with_sde_and_db(Arc::clone(&sde), Arc::clone(&db)));
    let dir = LivingWorldDirector::new();

    // Register 3 convoys in no_belt_sys: destroyed (skipped), already LOD 2 (skipped), LOD 1 MiningOp (materialized).
    dir.register_convoy(LivingWorldConvoy {
        convoy_id: 800_955_001,
        archetype: NpcArchetype::MiningOp,
        ship_type_id: 32880,
        origin_system_id: no_belt_sys,
        current_system_id: no_belt_sys,
        destination_system_id: no_belt_sys,
        destination_station_id: 60_955_001,
        cargo_type_id: 1230,
        cargo_quantity: 0,
        lod_level: 1,
        destroyed: true,
    });
    dir.register_convoy(LivingWorldConvoy {
        convoy_id: 800_955_002,
        archetype: NpcArchetype::PirateRoamer,
        ship_type_id: 16997,
        origin_system_id: no_belt_sys,
        current_system_id: no_belt_sys,
        destination_system_id: no_belt_sys,
        destination_station_id: 60_955_001,
        cargo_type_id: 34,
        cargo_quantity: 0,
        lod_level: 2,
        destroyed: false,
    });
    dir.register_convoy(LivingWorldConvoy {
        convoy_id: 800_955_003,
        archetype: NpcArchetype::MiningOp,
        ship_type_id: 32880,
        origin_system_id: no_belt_sys,
        current_system_id: no_belt_sys,
        destination_system_id: no_belt_sys,
        destination_station_id: 60_955_001,
        cargo_type_id: 1230,
        cargo_quantity: 0,
        lod_level: 1,
        destroyed: false,
    });
    assert_eq!(dir.materialize_system_lod2(no_belt_sys, &systems, &sde).await, 1);

    // step_all_hauler_convoys skips routes pointing to destroyed or non-HaulerConvoy convoys.
    dir.set_convoy_route(800_955_002, vec![no_belt_sys, 30_000_142]);
    assert!(dir.step_all_hauler_convoys(&systems, &db, &sde).await.is_empty());

    // step_npc_mining_op:
    // 1. Distance-based arrival (hb within 5_000m of dest_station_pos even when elapsed_tick_secs < 15.0)
    // 2. hold_full stacking onto existing Cargo item on hauler_ball_id when hauler_snap.is_some()
    let cid = 800_956_001_i64;
    let hauler_ball_id = cid + 1_000_000;
    let peri_sys = 30_000_144_u32;
    let dest_sta = 60_003_766_i64;
    let st_info = sde.solar_systems[&peri_sys]
        .stations
        .iter()
        .find(|s| s.station_id == dest_sta)
        .expect("seeded Perimeter station");
    let dest_sta_pos = st_info.position;
    let owner_corp = st_info.owner_corporation_id;

    dir.register_convoy(LivingWorldConvoy {
        convoy_id: cid,
        archetype: NpcArchetype::MiningOp,
        ship_type_id: 32880,
        origin_system_id: peri_sys,
        current_system_id: peri_sys,
        destination_system_id: peri_sys,
        destination_station_id: dest_sta,
        cargo_type_id: 1230,
        cargo_quantity: 49_999,
        lod_level: 1,
        destroyed: false,
    });

    // Spawn hauler at destination station with cargo, and step with elapsed=5s (< 15s) -> arrives by distance!
    let peri_handle = systems.get_or_spawn(peri_sys);
    let (hb, hc) = sde_ball_and_combat(&sde, hauler_ball_id, 648, dest_sta_pos + DVec3::new(1_000.0, 0.0, 0.0));
    peri_handle.spawn_ball_with_combat(hb, hc).await.unwrap();
    db.with_mut_snapshot(|snap| {
        snap.items.insert(
            70_001,
            InventoryItemRecord {
                item_id: 70_001,
                type_id: 1230,
                owner_id: owner_corp,
                location_id: hauler_ball_id,
                flag: InventoryFlag::Cargo,
                quantity: 200,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });
    // First call sets mining_last_step (elapsed=0), still arrives by distance <= 5000m!
    let r_dist = dir.step_npc_mining_op(cid, &systems, &db, &sde).await.unwrap();
    assert!(r_dist.hauled_to_station);

    // Now test hold_full stacking onto an existing cargo item when hauler_snap.is_some() and in_transit_cargo == 0:
    // Re-seed an item owned by owner_corp on hauler_ball_id with quantity == 0 (so hauler_cargo_units == 0),
    // and ensure hauler_snap is Some, then step with elapsed=15s so hold_full triggers and stacks onto item 70_002!
    let (hb2, hc2) = sde_ball_and_combat(&sde, hauler_ball_id, 648, dest_sta_pos + DVec3::new(100_000.0, 0.0, 0.0));
    peri_handle.spawn_ball_with_combat(hb2, hc2).await.unwrap();
    db.with_mut_snapshot(|snap| {
        snap.items.insert(
            70_002,
            InventoryItemRecord {
                item_id: 70_002,
                type_id: r_dist.ore_type_id,
                owner_id: owner_corp,
                location_id: hauler_ball_id,
                flag: InventoryFlag::Cargo,
                quantity: 0,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });
    clock.advance_secs(15);
    let r_stack = dir.step_npc_mining_op(cid, &systems, &db, &sde).await.unwrap();
    assert_eq!(r_stack.convoy_cargo_units, 0);
    db.with_snapshot(|snap| {
        assert!(snap.items[&70_002].quantity >= 50_000);
    });

    // Under-fire miner already within <= 5000m of dest_station_pos does NOT start warp!
    let _ = peri_handle
        .set_ball_position(cid, dest_sta_pos + DVec3::new(1_000.0, 0.0, 0.0))
        .await;
    {
        let mut bp = peri_handle.ballpark.write().unwrap();
        let cs = bp.combat_states.get_mut(&cid).unwrap();
        cs.shield_hp = 0.0;
    }
    clock.advance_secs(5);
    dir.step_npc_mining_op(cid, &systems, &db, &sde).await.unwrap();
    let (_, mb_near) = peri_handle.get_ball_snapshot(cid).await.unwrap().unwrap();
    assert!(!matches!(mb_near.mode, MovementMode::Warp { .. }));
}
