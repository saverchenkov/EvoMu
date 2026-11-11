//! Comprehensive unit tests for `living_world` covering mining escort hulls, LOD2 materialization,
//! under-fire evasion boundaries, hauler transit destruction/stacking, and warp physics clamping.

#![forbid(unsafe_code)]

use super::*;
use crate::system_actor::SystemRegistry;
use eve_db::clock::{ServerClock, VirtualServerClock};
use eve_db::{InventoryFlag, InventoryItemRecord, IskCents, MarketOrderRecord, WorldRepository};
use eve_destiny::MovementMode;
use eve_sde::SdeRegistry;
use std::sync::{Arc, OnceLock};

static SHARED_SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();

fn shared_sde() -> Arc<SdeRegistry> {
    Arc::clone(SHARED_SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

#[test]
fn escort_hulls_navy_patrols_convoy_destruction_and_transit_clamping() {
    let mut sde = (*shared_sde()).clone();
    let jita = sde.solar_systems.get(&30_000_142).unwrap().clone();
    let jita_const = sde.constellations.get(&jita.constellation_id).unwrap().clone();

    for (idx, (fac, exp_escort, exp_navy)) in [
        (500_001_u32, 16238_u32, 52_936_u32),
        (500_002, 16242, 52_959),
        (500_003, 16236, 52_927),
        (500_004, 16240, 52_950),
    ]
    .into_iter()
    .enumerate()
    {
        let cid = 20_910_000 + idx as u32;
        let sid = 30_910_000 + idx as u32;
        let mut c = jita_const.clone();
        c.constellation_id = cid;
        c.faction_id = Some(fac);
        sde.constellations.insert(cid, c);

        let mut sys = jita.clone();
        sys.solar_system_id = sid;
        sys.constellation_id = cid;
        sys.stations.clear();
        sde.solar_systems.insert(sid, sys);

        assert_eq!(mining_escort_type_for_system(&sde, sid), exp_escort);
        let dir = LivingWorldDirector::new();
        let nid = dir.ensure_system_navy_patrol(sid, &sde).expect("empire navy patrol");
        assert_eq!(dir.get_convoy(nid).unwrap().ship_type_id, exp_navy);
        let _ = idx;
    }
    assert_eq!(mining_escort_type_for_system(&sde, 99_999_999), 16238);

    // Transit duration physics: unknown ship fallback, slow (< 1.5 AU/s) & fast (> 12.0 AU/s) clamping, and 15 AU fallback
    let base_ship = sde.types.get(&648).unwrap().clone();
    let mut slow_ship = base_ship.clone();
    slow_ship.type_id = 995_001;
    slow_ship.default_attributes.insert(1281, 0.5);
    slow_ship.default_attributes.insert(600, 1.0); // 0.5 * 1.0 -> clamped to 1.5 AU/s
    sde.types.insert(995_001, slow_ship);

    let mut fast_ship = base_ship;
    fast_ship.type_id = 995_002;
    fast_ship.default_attributes.insert(1281, 6.0);
    fast_ship.default_attributes.insert(600, 5.0); // 30.0 -> clamped to 12.0 AU/s
    sde.types.insert(995_002, fast_ship);

    let t_unknown = LivingWorldDirector::compute_hauler_transit_duration_secs(&sde, 999_999_999, 99_999_999, None, None, 0);
    let t_slow = LivingWorldDirector::compute_hauler_transit_duration_secs(&sde, 995_001, 30_000_144, Some(30_000_142), None, 60_003_766);
    let t_fast = LivingWorldDirector::compute_hauler_transit_duration_secs(&sde, 995_002, 30_000_144, Some(30_000_142), None, 60_003_766);
    assert!(t_unknown >= 7);
    assert!(t_slow > t_fast);

    // mark_convoy_destroyed vs mark_convoy_destroyed_in_combat cooldown behavior + ensure_system_hauler_convoy cooldown respect
    let dir = LivingWorldDirector::new_seeded();
    assert_eq!(dir.list_convoys().len(), 3);
    dir.mark_convoy_destroyed(800_000_002); // MiningOp -> no hauler cooldown
    assert!(dir.hauler_cooldowns.read().unwrap().is_empty());
    dir.mark_convoy_destroyed_in_combat(800_000_001); // HaulerConvoy -> inserts cooldown for 30_000_144
    assert!(dir.hauler_cooldowns.read().unwrap().contains_key(&30_000_144));
    // Calling mark_convoy_destroyed_in_combat again on already-destroyed convoy is a safe no-op
    dir.mark_convoy_destroyed_in_combat(800_000_001);

    let hc_id = dir.ensure_system_hauler_convoy(30_000_142, &sde);
    dir.mark_convoy_destroyed(hc_id);
    // While cooldown active, ensure_system_hauler_convoy returns the destroyed convoy ID without recreating it!
    let hc_again = dir.ensure_system_hauler_convoy(30_000_142, &sde);
    assert_eq!(hc_again, hc_id);
    assert!(dir.get_convoy(hc_id).unwrap().destroyed);
}

#[tokio::test]
async fn materialize_lod2_and_mining_op_under_fire_evasion_and_cleanup() {
    let sde = shared_sde();
    let clock = Arc::new(VirtualServerClock::with_fixed_base(1_770_000_000));
    let db = Arc::new(WorldRepository::with_demo_seed());
    db.set_clock(clock.clone());
    let systems = Arc::new(SystemRegistry::with_sde_and_db(Arc::clone(&sde), Arc::clone(&db)));
    let dir = LivingWorldDirector::new();

    // Empty system returns 0
    assert_eq!(dir.materialize_system_lod2(30_000_144, &systems, &sde).await, 0);

    // Seed MiningOp + PirateRoamer in Perimeter (30_000_144) and materialize LOD2
    let cid = 800_600_001_i64;
    dir.register_convoy(LivingWorldConvoy {
        convoy_id: cid,
        archetype: NpcArchetype::MiningOp,
        ship_type_id: 32880,
        origin_system_id: 30_000_144,
        current_system_id: 30_000_144,
        destination_system_id: 30_000_144,
        destination_station_id: 60_003_766,
        cargo_type_id: 1230,
        cargo_quantity: 0,
        lod_level: 1,
        destroyed: false,
    });
    dir.register_convoy(LivingWorldConvoy {
        convoy_id: cid + 1,
        archetype: NpcArchetype::PirateRoamer,
        ship_type_id: 16997,
        origin_system_id: 30_000_144,
        current_system_id: 30_000_144,
        destination_system_id: 30_000_144,
        destination_station_id: 60_003_766,
        cargo_type_id: 34,
        cargo_quantity: 10,
        lod_level: 1,
        destroyed: false,
    });
    assert_eq!(dir.materialize_system_lod2(30_000_144, &systems, &sde).await, 2);

    let sh = systems.get_or_spawn(30_000_144);
    let hauler_id = cid + 1_000_000;
    let escort_id = cid + 2_000_000;
    assert!(sh.get_ball_snapshot(cid).await.unwrap().is_some());
    assert!(sh.get_ball_snapshot(hauler_id).await.unwrap().is_some());
    assert!(sh.get_ball_snapshot(escort_id).await.unwrap().is_some());

    // Error paths: missing convoy & non-MiningOp
    assert!(dir.step_npc_mining_op(999_999, &systems, &db, &sde).await.is_err());
    assert!(dir.step_npc_mining_op(cid + 1, &systems, &db, &sde).await.is_err());

    // Under-fire boundary: 0.5 shield loss (< 1.0 threshold) does NOT trigger warp evasion!
    {
        let mut bp = sh.ballpark.write().unwrap();
        let cs = bp.combat_states.get_mut(&cid).unwrap();
        cs.shield_hp = cs.max_shield_hp - 0.5;
    }
    dir.step_npc_mining_op(cid, &systems, &db, &sde).await.unwrap();
    let (_, mb_calm) = sh.get_ball_snapshot(cid).await.unwrap().unwrap();
    assert!(!matches!(mb_calm.mode, MovementMode::Warp { .. }));

    // Inflict 2.0 armor damage (> 1.0 threshold) -> both miner and hauler enter Warp toward station!
    {
        let mut bp = sh.ballpark.write().unwrap();
        let cs = bp.combat_states.get_mut(&cid).unwrap();
        cs.armor_hp = cs.max_armor_hp - 2.0;
    }
    clock.advance_secs(5);
    dir.step_npc_mining_op(cid, &systems, &db, &sde).await.unwrap();
    let (_, mb_evade) = sh.get_ball_snapshot(cid).await.unwrap().unwrap();
    let (_, hb_evade) = sh.get_ball_snapshot(hauler_id).await.unwrap().unwrap();
    assert!(matches!(mb_evade.mode, MovementMode::Warp { .. }));
    assert!(matches!(hb_evade.mode, MovementMode::Warp { .. }));

    // Destroyed MiningOp cleanup: removes miner, hauler, escort balls AND purges hauler cargo items!
    db.with_mut_snapshot(|snap| {
        snap.items.insert(
            99_101,
            InventoryItemRecord {
                item_id: 99_101,
                type_id: 1230,
                owner_id: 1_000_035,
                location_id: hauler_id,
                flag: InventoryFlag::Cargo,
                quantity: 500,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });
    assert_eq!(dir.hauler_cargo_units(&db, cid), 500);
    dir.mark_convoy_destroyed(cid);
    assert!(dir.step_npc_mining_op(cid, &systems, &db, &sde).await.is_err());
    assert!(sh.get_ball_snapshot(cid).await.unwrap().is_none());
    assert!(sh.get_ball_snapshot(hauler_id).await.unwrap().is_none());
    assert!(sh.get_ball_snapshot(escort_id).await.unwrap().is_none());
    assert_eq!(dir.hauler_cargo_units(&db, cid), 0);
}

#[tokio::test]
async fn mining_op_hauler_lost_in_transit_station_stacking_and_depleted_belts() {
    let mut sde = (*shared_sde()).clone();
    // Create a beltless synthetic system 30_920_001 to cover "No minable asteroids remaining"
    let mut beltless = sde.solar_systems.get(&30_000_144).unwrap().clone();
    beltless.solar_system_id = 30_920_001;
    sde.solar_systems.insert(30_920_001, beltless);
    let sde = Arc::new(sde);

    let clock = Arc::new(VirtualServerClock::with_fixed_base(1_780_000_000));
    let db = Arc::new(WorldRepository::new_empty());
    db.set_clock(clock.clone());
    let systems = Arc::new(SystemRegistry::with_sde_and_db(Arc::clone(&sde), Arc::clone(&db)));
    let dir = LivingWorldDirector::new();

    // 1. Beltless system -> cleans up balls and returns "No minable asteroids remaining"
    let empty_cid = 800_700_001_i64;
    dir.register_convoy(LivingWorldConvoy {
        convoy_id: empty_cid,
        archetype: NpcArchetype::MiningOp,
        ship_type_id: 32880,
        origin_system_id: 30_920_001,
        current_system_id: 30_920_001,
        destination_system_id: 30_920_001,
        destination_station_id: 0,
        cargo_type_id: 1230,
        cargo_quantity: 0,
        lod_level: 1,
        destroyed: false,
    });
    let err = dir.step_npc_mining_op(empty_cid, &systems, &db, &sde).await.unwrap_err();
    assert!(err.contains("No minable asteroids remaining"));

    // 2. Real belt system: hauler destroyed in transit (`in_transit_cargo > 0 && hauler_snap.is_none()`),
    // then `hold_full` load + 15s arrival stacking onto existing station hangar item & regional ask order!
    let cid = 800_700_002_i64;
    let hauler_id = cid + 1_000_000;
    let dest_sta = 60_003_766_i64;
    dir.register_convoy(LivingWorldConvoy {
        convoy_id: cid,
        archetype: NpcArchetype::MiningOp,
        ship_type_id: 32880,
        origin_system_id: 30_000_144,
        current_system_id: 30_000_144,
        destination_system_id: 30_000_144,
        destination_station_id: dest_sta,
        cargo_type_id: 1230,
        cargo_quantity: 49_999, // 49_999 * 0.1 = 4_999.9 m3 -> 1 tick fills 5,000 m3 hold!
        lod_level: 1,
        destroyed: false,
    });

    // Seed orphan hauler cargo while hauler ball does NOT exist -> purged on first step, while hold_full loads fresh cargo!
    db.with_mut_snapshot(|snap| {
        snap.items.insert(
            77_001,
            InventoryItemRecord {
                item_id: 77_001,
                type_id: 1230,
                owner_id: 1_000_035,
                location_id: hauler_id,
                flag: InventoryFlag::Cargo,
                quantity: 333,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        // Existing station hangar stack (`owner_id == 1`) + regional sell order to exercise min-price & order creation
        snap.items.insert(
            77_002,
            InventoryItemRecord {
                item_id: 77_002,
                type_id: 1230,
                owner_id: 1,
                location_id: dest_sta,
                flag: InventoryFlag::Hangar,
                quantity: 1_000,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        snap.market_orders.insert(
            88_001,
            MarketOrderRecord {
                order_id: 88_001,
                owner_id: 90_000_001,
                station_id: 60_003_760,
                region_id: 10_000_002,
                type_id: 1230,
                is_bid: false,
                price: IskCents::from_isk(17),
                vol_entered: 500,
                vol_remaining: 500,
                min_volume: 1,
                is_npc: false,
                escrow: IskCents(0),
            },
        );
    });

    // Step 1 (t=0): purges orphan 333 cargo, mines 0 units, cargo stays 49_999
    let r1 = dir.step_npc_mining_op(cid, &systems, &db, &sde).await.unwrap();
    assert_eq!(r1.mined_units, 0);
    assert_eq!(dir.hauler_cargo_units(&db, cid), 0);

    // Step 2 (t=+15s): mines >= 1 unit -> hold_full -> loads onto hauler & spawns hauler warping to station!
    clock.advance_secs(15);
    let r2 = dir.step_npc_mining_op(cid, &systems, &db, &sde).await.unwrap();
    assert!(r2.mined_units >= 1);
    assert_eq!(r2.convoy_cargo_units, 0);
    assert!(dir.hauler_cargo_units(&db, cid) >= 50_000);

    // Step 3 (t=+35s, elapsed 20s >= 15s): hauler arrives via `step_all_mining_ops`, stacks onto 77_002 & creates NPC ask order at 17 ISK!
    clock.advance_secs(20);
    let all_res = dir.step_all_mining_ops(&systems, &db, &sde).await;
    let (_, r3) = all_res.into_iter().find(|(id, _)| *id == cid).unwrap();
    let r3 = r3.unwrap();
    assert!(r3.hauled_to_station);
    db.with_snapshot(|snap| {
        assert!(snap.items[&77_002].quantity >= 51_000);
        let npc_ord = snap
            .market_orders
            .values()
            .find(|o| o.is_npc && o.station_id == dest_sta && o.type_id == 1230)
            .expect("created NPC ask order");
        assert_eq!(npc_ord.price, IskCents::from_isk(17));
    });
}

#[tokio::test]
async fn mining_op_depletion_cleanup_and_hauler_cooldown_respawn_guard() {
    let sde = shared_sde();
    let clock = Arc::new(VirtualServerClock::with_fixed_base(1_770_000_000));
    let db = Arc::new(WorldRepository::with_demo_seed());
    db.set_clock(clock.clone());
    let dir = LivingWorldDirector::new();

    // Unknown faction fallback in ensure_system_navy_patrol returns None
    let mut custom_sde = (*sde).clone();
    if let Some(sys) = custom_sde.solar_systems.get_mut(&30_000_142) {
        sys.stations.clear();
        if let Some(c) = custom_sde.constellations.get_mut(&sys.constellation_id) {
            c.faction_id = Some(500_999);
        }
        if let Some(r) = custom_sde.regions.get_mut(&sys.region_id) {
            r.faction_id = Some(500_999);
        }
    }
    assert_eq!(dir.ensure_system_navy_patrol(30_000_142, &custom_sde), None);

    // Mining op with 0 remaining minable asteroids removes companion hauler/miner/escort balls
    let cid = 800_700_001_i64;
    let hauler_ball_id = cid + 1_000_000;
    let sys_id = 30_000_144_u32;
    let dest_sta = sde.solar_systems[&sys_id].stations[0].station_id;
    let mut no_sys_sde = (*sde).clone();
    no_sys_sde.solar_systems.remove(&sys_id);
    let no_belt_systems = Arc::new(SystemRegistry::with_sde_and_db(
        Arc::new(no_sys_sde),
        Arc::clone(&db),
    ));
    let handle = no_belt_systems.get_or_spawn(sys_id);
    let (hb, hc) = sde_ball_and_combat(&sde, hauler_ball_id, 648, glam::DVec3::ZERO);
    let _ = handle.spawn_ball_with_combat(hb, hc).await;
    dir.register_convoy(LivingWorldConvoy {
        convoy_id: cid,
        archetype: NpcArchetype::MiningOp,
        ship_type_id: 32880,
        origin_system_id: sys_id,
        current_system_id: sys_id,
        destination_system_id: sys_id,
        destination_station_id: dest_sta,
        cargo_type_id: 1230,
        cargo_quantity: 0,
        lod_level: 2,
        destroyed: false,
    });
    assert!(dir
        .step_npc_mining_op(cid, &no_belt_systems, &db, &sde)
        .await
        .is_err());
    assert!(handle.get_ball_snapshot(hauler_ball_id).await.unwrap().is_none());

    // mark_convoy_destroyed_in_combat on HaulerConvoy + ensure_system_hauler_convoy with expired cooldown
    let hc_id = dir.ensure_system_hauler_convoy(30_000_142, &sde);
    dir.mark_convoy_destroyed_in_combat(hc_id);
    assert_eq!(dir.ensure_system_hauler_convoy(30_000_142, &sde), hc_id);
}
