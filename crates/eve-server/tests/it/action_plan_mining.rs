//! Acceptance test suite for Mining & Reprocessing Canonical Tasks (`Phase 5M`: `C-6`, `MN-01`..`MN-12`).
//!
//! Validates canonical tasks `plan:C-6` and `plan:MN-01` through `plan:MN-12`
//! against the authoritative "Done when" acceptance criteria in `action_plan.md`.

#![forbid(unsafe_code)]

use crate::common;
use eve_db::{
    AsteroidRecord, InventoryFlag, InventoryItemRecord, TrainedSkillRecord, UpwellStructureRecord,
};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::services::mining_cycle::{process_mining_cycles, MiningStopReason};
use eve_server::system_actor::AsteroidState;
use glam::DVec3;
use std::collections::HashMap;

pub(crate) fn get_dict_val<'a>(rep: &'a PyRep, key: &str) -> Option<&'a PyRep> {
    match rep {
        PyRep::Dict(entries) => entries.iter().find_map(|(k, v)| match k {
            PyRep::String(s) if s == key => Some(v),
            _ => None,
        }),
        PyRep::KeyVal(entries) => entries.get(key),
        _ => None,
    }
}

pub(crate) fn make_item(item_id: i64, type_id: u32, owner_id: i64, location_id: i64, flag: InventoryFlag, quantity: i32, singleton: bool) -> InventoryItemRecord {
    InventoryItemRecord { item_id, type_id, owner_id, location_id, flag, quantity, singleton, version: 1, ..Default::default() }
}

pub(crate) fn make_structure(structure_id: i64, owner_corp_id: i64, type_id: u32, solar_system_id: u32, name: &str, position: [f64; 3]) -> UpwellStructureRecord {
    UpwellStructureRecord {
        structure_id, owner_corp_id, type_id, name: name.into(), solar_system_id, position,
        state: "ShieldVulnerable".into(), quantum_core_installed: true, fuel_blocks: 100,
        shield_hp: 1000.0, armor_hp: 1000.0, hull_hp: 1000.0,
        reinforced_until_unix: 0, last_damage_tick: 0, last_damage_unix: 0, self_destruct_at_unix: None,
    }
}

pub(crate) async fn setup_pilot_session(ctx: &ServerContext, char_id: i64) -> SessionState {
    let mut sess = SessionState::new(1001);
    let kw = HashMap::new();
    common::login_pilot(ctx, &mut sess, "demo", "demo-secret").await;
    ctx.dispatch_rpc(&mut sess, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(char_id)], &kw, None).await.expect("SelectCharacterID");
    sess
}

// ====================================================================
// Test 1: Data, Belts, Lasers, Holds, Residue, Dogma Bonuses (MN-03..07)
// ====================================================================
#[tokio::test]
async fn test_action_plan_c5_mn01_to_mn07_mining_data_belts_lasers_holds_crystals_bonuses() {
    // parity: plan:MN-03, plan:MN-04, plan:MN-05, plan:MN-06, plan:MN-07
    eve_testkit::attest(&["plan:MN-03", "plan:MN-04", "plan:MN-05", "plan:MN-06", "plan:MN-07"]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let kw = HashMap::new();
    let char_id = 90_000_001i64;
    let mut sess = setup_pilot_session(&ctx, char_id).await;

    // 1. Real SDE Static Data & Belt Ore Distribution (ores, variants, moon ores, ice, gas, compressible_types, dogma attrs)
    assert_eq!((ctx.sde.types[&1230].portion_size, ctx.sde.type_materials[&1230].len(), ctx.sde.type_materials[&1230][0].material_type_id, ctx.sde.type_materials[&1230][0].quantity), (100, 1, 34, 400));
    assert_eq!(ctx.sde.type_materials[&1228].len(), 2, "Scordite yields Tritanium + Pyerite");
    assert_eq!((ctx.sde.type_materials[&17470][0].quantity, ctx.sde.type_materials[&17471][0].quantity, ctx.sde.type_materials[&46689][0].quantity), (420, 440, 460));
    assert!(!ctx.sde.type_materials[&45490].is_empty() && !ctx.sde.type_materials[&45513].is_empty() && ctx.sde.types[&30370].group_id == 711);
    for mat_tid in [16272u32, 16273, 16274, 16275] {
        assert!(ctx.sde.type_materials[&16262].iter().any(|m| m.material_type_id == mat_tid), "Clear Icicle yields {mat_tid}");
    }
    assert_eq!((ctx.sde.compressible_types[&1230], ctx.sde.types[&32880].default_attributes[&1556]), (62516, 5_000.0));
    assert!(ctx.sde.compressible_types.contains_key(&16262) && ctx.sde.types[&483].default_attributes.contains_key(&77));

    let jita_system_id = 30000142u32;
    let jita = ctx.systems.get_or_spawn(jita_system_id);
    assert!(!ctx.sde.celestials_in_system(jita_system_id).is_empty(), "Jita system celestials exist in SDE");
    let mut hs_bp = eve_destiny::Ballpark::new(30000144);
    let perim_belt_states = eve_server::system_actor::populate_asteroid_belt_entities(&mut hs_bp, 30000144, &ctx.sde);
    assert!(!perim_belt_states.is_empty() && perim_belt_states.iter().all(|st| matches!(st.type_id, 1230 | 1228 | 1224 | 18)));
    let mut nullsec_bp = eve_destiny::Ballpark::new(30000208);
    let nullsec_states = eve_server::system_actor::populate_asteroid_belt_entities(&mut nullsec_bp, 30000208, &ctx.sde);
    assert!(nullsec_states.iter().any(|s| s.type_id == 11396) && nullsec_states.iter().any(|s| matches!(s.type_id, 1229 | 1232)));

    // 3. plan:MN-04: Venture Miner I cycle, exact SDE m³, notifications & hold-full stop
    let undock = ctx.dispatch_rpc(&mut sess, "stationSvc", None, "Undock", &[], &kw, None).await.expect("Undock");
    let ship_id = get_dict_val(&undock, "shipID").unwrap().as_i64().unwrap();
    let (_, ship_ball) = jita.get_ball_snapshot(ship_id).await.unwrap().unwrap();

    let miner_mod_id = 800_000_501i64;
    let rock_id = 700_000_501i64;
    let rock_qty = 5_000i64;
    let rock_pos = ship_ball.position + DVec3::new(1000.0, 0.0, 0.0);

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ship) = snap.items.get_mut(&ship_id) {
            ship.type_id = 32880; // Venture
        }
        snap.items.retain(|_, item| item.location_id != ship_id);
        snap.items.insert(miner_mod_id, make_item(miner_mod_id, 483, char_id, ship_id, InventoryFlag::HiSlot0, 1, true));
        snap.module_online_states.insert(miner_mod_id, true);
        let sk = snap.skills.entry(char_id).or_default();
        sk.insert(3386, TrainedSkillRecord { type_id: 3386, level: 5, skill_points: 256_000.0 }); // Mining V
        sk.insert(32918, TrainedSkillRecord { type_id: 32918, level: 5, skill_points: 256_000.0 }); // Mining Frigate V
        sk.insert(3410, TrainedSkillRecord { type_id: 3410, level: 5, skill_points: 256_000.0 }); // Astrogeology V
        sk.insert(17940, TrainedSkillRecord { type_id: 17940, level: 5, skill_points: 256_000.0 }); // Mining Barge V
        sk.insert(22578, TrainedSkillRecord { type_id: 22578, level: 5, skill_points: 256_000.0 }); // Mining Upgrades V
        sk.insert(18025, TrainedSkillRecord { type_id: 18025, level: 5, skill_points: 256_000.0 }); // Veldspar Processing V
        sk.insert(60377, TrainedSkillRecord { type_id: 60377, level: 5, skill_points: 256_000.0 }); // Simple Ore Processing V
        sk.insert(3429, TrainedSkillRecord { type_id: 3429, level: 5, skill_points: 256_000.0 }); // Target Management V
    });
    ctx.invalidate_ship_fitting(ship_id);

    ctx.db.spawn_asteroid(AsteroidRecord {
        asteroid_id: rock_id, solar_system_id: jita_system_id, belt_id: 40_000_001, type_id: 1230,
        quantity: rock_qty, initial_quantity: rock_qty, position: (rock_pos.x, rock_pos.y, rock_pos.z), radius: 50.0,
    });
    jita.spawn_asteroid(
        AsteroidState {
            asteroid_id: rock_id, belt_id: 40_000_001, type_id: 1230, initial_units: rock_qty as u64,
            remaining_units: rock_qty as u64, unit_volume: 0.1, depleted: false, respawn_at_tick: None,
        },
        Ball {
            id: rock_id, type_id: 1230, position: rock_pos, velocity: DVec3::ZERO, max_velocity: 0.0,
            inertia: 0.0, mass: 1.0e10, radius: 50.0, speed_fraction: 0.0, mode: MovementMode::Stop,
        },
    )
    .await
    .unwrap();

    // Failed activation before locking target must return TargetNotLocked
    let unlocked_err = ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(miner_mod_id), PyRep::String("miningLaser".into()), PyRep::Integer(rock_id)], &kw, None).await.expect_err("Activate without target lock must fail");
    assert_eq!(unlocked_err.code, "TargetNotLocked");

    // Lock target and activate Miner I
    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "AddTarget", &[PyRep::Integer(rock_id)], &kw, None).await.expect("AddTarget");
    for _ in 0..30 {
        if jita.get_target_locks(ship_id).await.unwrap().iter().any(|l| l.target_id == rock_id && l.locked) { break; }
        jita.step_manual(1.0).await.unwrap();
    }

    ctx.drain_notifications_for_char(char_id);
    let act_res = ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(miner_mod_id), PyRep::String("miningLaser".into()), PyRep::Integer(rock_id)], &kw, None).await.expect("Activate Miner I");
    assert_eq!(get_dict_val(&act_res, "cycleTimeMs").and_then(|v| v.as_i64()), Some(15_000));

    // Seed RNG for no crit (0.01 crit chance) and resolve 1 full 15s cycle:
    // Venture + Miner I + Mining V + Astrogeology V + Mining Frigate V = 10 * 1.25 * 1.25 * 1.25 * 2.0 = 39.0625 m³ = 390 units of Veldspar (0.1 m³)
    ctx.set_mechanic_rng_seed(Some(crate::mining_cycle_engine::seed_for(&[false], 0.01)));
    ctx.advance_virtual_clock(15);
    let cyc_out = process_mining_cycles(&ctx).await;
    assert_eq!(cyc_out.len(), 1);
    assert_eq!(cyc_out[0].mining_amount_m3, 39.0625, "Server-computed SDE yield must be 39.0625 m³");
    assert_eq!(cyc_out[0].units_delivered, 390, "39.0625 m³ / 0.1 m³ = 390 Veldspar units");
    assert_eq!(cyc_out[0].residue_units, 0, "T1 Miner I has 0 residue");
    assert!(ctx.drain_notifications_for_char(char_id).iter().any(|n| n.event == "OnOreMined"), "OnOreMined must fire");

    let ore_units: i32 = ctx.db.with_snapshot(|snap| {
        snap.items.values().filter(|i| i.location_id == ship_id && i.flag == InventoryFlag::SpecializedOreHold && i.type_id == 1230).map(|i| i.quantity).sum()
    });
    assert_eq!(ore_units, 390, "SpecializedOreHold must contain exact 390 mined Veldspar units");

    // Hold-full clamping & automatic deactivation: fill Venture's 5,000 m³ (50,000 unit) ore hold to 49,800 units (200 free)
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(st) = snap.items.values_mut().find(|i| i.location_id == ship_id && i.flag == InventoryFlag::SpecializedOreHold) { st.quantity = 49_800; }
    });
    ctx.set_mechanic_rng_seed(Some(crate::mining_cycle_engine::seed_for(&[false], 0.01)));
    ctx.advance_virtual_clock(15);
    let full_out = process_mining_cycles(&ctx).await;
    assert_eq!((full_out.len(), full_out[0].units_delivered, full_out[0].stop), (1, 200, Some(MiningStopReason::HoldFull)));
    assert!(!ctx.mining_cycles.read().unwrap().contains_key(&miner_mod_id), "Laser must deactivate when hold is full");

    // 4. plan:MN-05: Hold type restrictions and capacity limits
    let mineral_id = 800_000_510i64;
    let oversized_id = 800_000_511i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(mineral_id, make_item(mineral_id, 34, char_id, ship_id, InventoryFlag::Cargo, 100, false));
        snap.items.insert(oversized_id, make_item(oversized_id, 1230, char_id, ship_id, InventoryFlag::Cargo, 60_000, false));
    });
    let min_err = ctx.dispatch_rpc(&mut sess, "invbroker", None, "Add", &[PyRep::Integer(mineral_id), PyRep::Integer(ship_id), PyRep::Integer(InventoryFlag::SpecializedOreHold as i64)], &kw, None).await.expect_err("Minerals rejected by SpecializedOreHold");
    assert_eq!(min_err.code, "InvalidItemTypeForHold");
    let cap_err = ctx.dispatch_rpc(&mut sess, "invbroker", None, "Add", &[PyRep::Integer(oversized_id), PyRep::Integer(ship_id), PyRep::Integer(InventoryFlag::SpecializedOreHold as i64)], &kw, None).await.expect_err("Oversized ore rejected by SpecializedOreHold");
    assert_eq!(cap_err.code, "CapacityExceeded");

    // 5. plan:MN-03: Depletion to 0, OnAsteroidTerminated, ball removal, and respawn tick
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.retain(|_, i| !(i.location_id == ship_id && i.flag == InventoryFlag::SpecializedOreHold));
        if let Some(a) = snap.asteroids.get_mut(&rock_id) {
            a.quantity = 200; // < 312 per cycle -> depletes to 0 on next cycle
        }
    });
    ctx.drain_notifications_for_char(char_id);
    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(miner_mod_id), PyRep::String("miningLaser".into()), PyRep::Integer(rock_id)], &kw, None)
        .await
        .expect("Activate on 200-unit rock");
    ctx.set_mechanic_rng_seed(Some(crate::mining_cycle_engine::seed_for(&[false], 0.01)));
    ctx.advance_virtual_clock(15);
    let dep_out = process_mining_cycles(&ctx).await;
    assert_eq!(dep_out[0].units_removed, 200);
    assert_eq!(dep_out[0].asteroid_remaining, 0);
    assert_eq!(dep_out[0].stop, Some(MiningStopReason::AsteroidDepleted));
    assert!(jita.get_ball_snapshot(rock_id).await.unwrap().is_none(), "Depleted asteroid ball removed from Ballpark");
    assert!(
        ctx.drain_notifications_for_char(char_id)
            .iter()
            .any(|n| n.event == "OnAsteroidTerminated" && n.args == vec![PyRep::Integer(rock_id)]),
        "Depleting asteroid to 0 must emit OnAsteroidTerminated(rock_id)"
    );

    // Warp-to-belt automatic asteroid spawning, dogmaIM.Activate belt depletion to 0, and maintenance respawn tick on Perimeter (30000144)
    let perim = ctx.systems.get_or_spawn(30_000_144);
    let first_belt_cel = ctx.sde.celestials_in_system(30_000_144).into_iter().find(|c| c.group_id == 9).expect("Perimeter SDE belt");
    let first_belt_id = first_belt_cel.celestial_id;
    let sample_ast_id = 750_000_000i64;
    perim.spawn_ball(Ball { id: ship_id, type_id: 17478, position: first_belt_cel.position + DVec3::new(200_000.0, 0.0, 0.0), velocity: DVec3::ZERO, max_velocity: 335.0, inertia: 2.8, mass: 1.2e6, radius: 30.0, speed_fraction: 0.0, mode: MovementMode::Stop }).await.unwrap();
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().solar_system_id = 30_000_144;
        snap.items.get_mut(&ship_id).unwrap().type_id = 17478; // Retriever (27,500 m³ Ore Hold)
        snap.items.retain(|_, i| i.location_id != ship_id);
        snap.items.insert(miner_mod_id, make_item(miner_mod_id, 17482, char_id, ship_id, InventoryFlag::HiSlot0, 1, true)); // Strip Miner I
        snap.items.insert(800_000_518, make_item(800_000_518, 22542, char_id, ship_id, InventoryFlag::LoSlot0, 1, true)); // MLU II
        snap.items.insert(800_000_519, make_item(800_000_519, 22542, char_id, ship_id, InventoryFlag::LoSlot1, 1, true)); // MLU II
        for id in [miner_mod_id, 800_000_518, 800_000_519] { snap.module_online_states.insert(id, true); }
    });
    ctx.invalidate_ship_fitting(ship_id);
    sess.solar_system_id = Some(30_000_144);
    let PyRep::Tuple(b_tup) = ctx.dispatch_rpc(&mut sess, "beyonce", None, "MachoBindObject", &[PyRep::Integer(30_000_144), PyRep::None], &kw, None).await.expect("Bind beyonce") else { panic!() };
    ctx.drain_notifications_for_char(char_id);
    ctx.dispatch_rpc(&mut sess, "beyonce", Some(b_tup[0].as_i64().unwrap() as u64), "CmdWarpToStuff", &[PyRep::String("item".into()), PyRep::Integer(first_belt_id), PyRep::Real(0.0)], &kw, None).await.expect("CmdWarpToStuff to belt");
    let notifs = ctx.drain_notifications_for_char(char_id);
    assert!(notifs.iter().any(|n| n.event == "OnWarpStarted"), "Warping to belt must emit OnWarpStarted");
    assert!(!notifs.iter().any(|n| n.event == "OnWarpToAsteroidBelt"), "OnWarpToAsteroidBelt is client_local and must not be emitted by server");
    for _ in 0..30 {
        let _ = perim.step_manual(1.0).await.unwrap();
        if perim.get_ball_snapshot(ship_id).await.unwrap().unwrap().1.position.distance(first_belt_cel.position) < 5_000.0 { break; }
    }
    let sample_init = ctx.db.get_asteroid(sample_ast_id).expect("CmdWarpToStuff must populate belt AsteroidRecord in ctx.db").quantity as u64;
    assert_eq!(sample_init, 20_000);
    for belt_ast_id in 750_000_000i64..=750_000_003 {
        let (_, ast_b) = perim.get_ball_snapshot(belt_ast_id).await.unwrap().expect("Belt asteroid ball spawned by CmdWarpToStuff");
        let _ = perim.set_ball_position(ship_id, ast_b.position + DVec3::new(500.0, 0.0, 0.0)).await;
        ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "AddTarget", &[PyRep::Integer(belt_ast_id)], &kw, None).await.expect("Lock belt asteroid");
        for _ in 0..30 {
            if perim.get_target_locks(ship_id).await.unwrap().iter().any(|l| l.target_id == belt_ast_id && l.locked) { break; }
            perim.step_manual(1.0).await.unwrap();
        }
        let _ = perim.set_capacitor(ship_id, 437.5).await;
        ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(miner_mod_id), PyRep::String("miningLaser".into()), PyRep::Integer(belt_ast_id)], &kw, None).await.expect("Activate Strip Miner I on belt asteroid");
        for _ in 0..3 {
            if ctx.db.get_asteroid(belt_ast_id).unwrap().quantity == 0 { break; }
            let _ = perim.set_capacitor(ship_id, 437.5).await;
            ctx.set_mechanic_rng_seed(Some(crate::mining_cycle_engine::seed_for(&[false], 0.01)));
            ctx.advance_virtual_clock(180);
            let _ = process_mining_cycles(&ctx).await;
        }
        assert_eq!(ctx.db.get_asteroid(belt_ast_id).unwrap().quantity, 0, "Belt asteroid {belt_ast_id} depleted in ctx.db");
        assert!(perim.get_ball_snapshot(belt_ast_id).await.unwrap().is_none(), "Depleted belt asteroid {belt_ast_id} removed from Ballpark");
    }
    let _ = perim.step_manual(1.0).await.unwrap();
    assert!(perim.get_ball_snapshot(sample_ast_id).await.unwrap().is_none(), "Stays gone before respawn interval");
    ctx.advance_secs(eve_server::system_actor::DEFAULT_BELT_RESPAWN_INTERVAL_SECS * 2);
    let _ = perim.step_manual(1.0).await.unwrap();
    assert_eq!(perim.get_asteroid_state(sample_ast_id).await.unwrap().unwrap().remaining_units, sample_init, "Respawn tick restores AsteroidState to initial units");
    assert_eq!(ctx.db.get_asteroid(sample_ast_id).unwrap().quantity, sample_init as i64, "Respawn tick restores AsteroidRecord in ctx.db to initial units");
    assert!(perim.get_ball_snapshot(sample_ast_id).await.unwrap().is_some(), "Respawned ball reappears in Ballpark");
    ctx.db.with_mut_snapshot(|snap| snap.characters.get_mut(&char_id).unwrap().solar_system_id = jita_system_id);
    sess.solar_system_id = Some(jita_system_id);

    // 6. plan:MN-05 (cargo fallback) & plan:MN-07: Non-mining hull routes to Cargo; Retriever + 2 MLU II; Modulated Strip Miner II + Crystal LoadAmmo, residue & shattering at 0 HP
    let strip_id = 800_000_520i64;
    let mlu_a = 800_000_521i64;
    let mlu_b = 800_000_522i64;
    let big_rock_id = 700_000_502i64;
    ctx.db.spawn_asteroid(AsteroidRecord {
        asteroid_id: big_rock_id, solar_system_id: jita_system_id, belt_id: 40_000_001, type_id: 1230,
        quantity: 500_000, initial_quantity: 500_000, position: (rock_pos.x, rock_pos.y, rock_pos.z), radius: 100.0,
    });
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ship) = snap.items.get_mut(&ship_id) { ship.type_id = 606; } // Velator (non-mining hull -> Cargo fallback)
        snap.items.retain(|_, i| i.location_id != ship_id);
        snap.items.insert(miner_mod_id, make_item(miner_mod_id, 483, char_id, ship_id, InventoryFlag::HiSlot0, 1, true));
    });
    ctx.invalidate_ship_fitting(ship_id);
    ctx.set_mechanic_rng_seed(Some(crate::mining_cycle_engine::seed_for(&[false], 0.01)));
    let fallback_cyc = crate::mining_cycle_engine::mine_once(&ctx, &mut sess, miner_mod_id, big_rock_id).await.expect("Velator cargo fallback cycle");
    assert_eq!(fallback_cyc.destination_flag, InventoryFlag::Cargo, "Non-mining hull must route mined ore to regular Cargo");

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ship) = snap.items.get_mut(&ship_id) { ship.type_id = 17478; } // Retriever
        snap.items.retain(|_, i| i.location_id != ship_id);
        snap.items.insert(strip_id, make_item(strip_id, 17482, char_id, ship_id, InventoryFlag::HiSlot0, 1, true)); // Strip Miner I
        snap.items.insert(mlu_a, make_item(mlu_a, 28576, char_id, ship_id, InventoryFlag::LoSlot0, 1, true)); // MLU II
        snap.items.insert(mlu_b, make_item(mlu_b, 28576, char_id, ship_id, InventoryFlag::LoSlot1, 1, true)); // MLU II
    });
    ctx.invalidate_ship_fitting(ship_id);

    ctx.set_mechanic_rng_seed(Some(crate::mining_cycle_engine::seed_for(&[false], 0.01)));
    let mlu_cycle = crate::mining_cycle_engine::mine_once(&ctx, &mut sess, strip_id, big_rock_id)
        .await
        .expect("Retriever Strip Miner I + 2 MLU II cycle");
    let published_yield = 150.0 * 1.25 * 1.25 * 1.15 * 1.10 * 1.09 * 1.09; // 352.2531 m³
    assert!((mlu_cycle.mining_amount_m3 - published_yield).abs() / published_yield < 0.001, "Server-computed dogma yield {} must match published {}", mlu_cycle.mining_amount_m3, published_yield);
    assert_eq!(mlu_cycle.units_delivered, 3_522, "Server-computed Strip Miner I + 2 MLU II delivers 3,522 Veldspar units");

    // plan:MN-07 implants (Highwall MX-1005 22535 +5% & Michi's Excavation Augmentor 20700 +5%) and Mining Foreman Burst II (43551) charges (42830 Optimization, 42829 Field Enhancement)
    let (burst_opt, burst_fld) = (800_000_524i64, 800_000_527i64);
    ctx.db.with_mut_snapshot(|snap| {
        snap.active_implants.entry(char_id).or_default().extend([22_535, 20_700]);
        let sk = snap.skills.entry(char_id).or_default();
        for sid in [3_348u32, 22_536, 22_552] { sk.insert(sid, TrainedSkillRecord { type_id: sid, level: 5, skill_points: 256_000.0 }); }
        snap.items.insert(burst_opt, make_item(burst_opt, 43_551, char_id, ship_id, InventoryFlag::HiSlot1, 1, true));
        snap.items.insert(800_000_525, make_item(800_000_525, 42_830, char_id, ship_id, InventoryFlag::HiSlot1, 10, false));
        snap.items.insert(burst_fld, make_item(burst_fld, 43_551, char_id, ship_id, InventoryFlag::HiSlot2, 1, true));
        snap.items.insert(800_000_526, make_item(800_000_526, 42_829, char_id, ship_id, InventoryFlag::HiSlot2, 10, false));
    });
    ctx.invalidate_ship_fitting(ship_id);
    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(burst_opt), PyRep::String("moduleBonusWarfareLinkMining".into())], &kw, None).await.expect("Activate Optimization Burst");
    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(burst_fld), PyRep::String("moduleBonusWarfareLinkMining".into())], &kw, None).await.expect("Activate Field Burst");
    let max_cap = jita.get_combat_state(ship_id).await.unwrap().unwrap().max_capacitor;
    jita.set_capacitor(ship_id, max_cap).await.unwrap();
    jita.spawn_ball(Ball { id: ship_id, type_id: 17478, position: rock_pos + DVec3::new(1000.0, 0.0, 0.0), velocity: DVec3::ZERO, max_velocity: 130.0, inertia: 1.0, mass: 20e6, radius: 150.0, speed_fraction: 0.0, mode: MovementMode::Stop }).await.unwrap();
    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "AddTarget", &[PyRep::Integer(big_rock_id)], &kw, None).await.unwrap();
    for _ in 0..30 { jita.step_manual(1.0).await.unwrap(); }
    let act_burst = ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(strip_id), PyRep::String("miningLaser".into()), PyRep::Integer(big_rock_id)], &kw, None).await.expect("Activate Strip Miner I under Mining Foreman Burst");
    assert_eq!(get_dict_val(&act_burst, "cycleTimeMs").and_then(|v| v.as_i64()), Some((45_000.0 * (1.0 - 0.15 * 1.25 * 1.5)) as i64), "Optimization burst (42830) reduces cycleTimeMs by 28.125%");
    assert_eq!(get_dict_val(&act_burst, "maxRange").and_then(|v| v.as_f64()), Some(15_000.0 * 1.75), "Field Enhancement burst (42829) increases maxRange by +75%");
    let _ = ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Deactivate", &[PyRep::Integer(strip_id), PyRep::String("miningLaser".into())], &kw, None).await;
    jita.set_capacitor(ship_id, max_cap).await.unwrap();
    ctx.set_mechanic_rng_seed(Some(crate::mining_cycle_engine::seed_for(&[false], 0.01)));
    let imp_cyc = crate::mining_cycle_engine::mine_once(&ctx, &mut sess, strip_id, big_rock_id).await.expect("Implant + burst cycle");
    assert!((imp_cyc.mining_amount_m3 - published_yield * 1.05 * 1.05).abs() < 1e-6, "Highwall MX-1005 + Michi's Excavation Augmentor scale yield by 1.05 * 1.05");
    assert_eq!(imp_cyc.units_delivered, 3_883);
    ctx.db.with_mut_snapshot(|snap| { snap.active_implants.remove(&char_id); snap.items.remove(&burst_opt); snap.items.remove(&burst_fld); });
    ctx.advance_secs(100);
    jita.set_capacitor(ship_id, max_cap).await.unwrap();

    // Modulated Strip Miner II (17912) loaded via dogmaIM.LoadAmmo with Simple Asteroid Mining Crystal Type B I (60279):
    // 1.5x yield, 0.9x duration, 54% waste probability, 1.0x waste multiplier, crystal volatility damage & shattering at 0 HP
    let crystal_id = 800_000_523i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.remove(&mlu_a);
        snap.items.remove(&mlu_b);
        if let Some(m) = snap.items.get_mut(&strip_id) { m.type_id = 17912; }
        snap.items.insert(crystal_id, make_item(crystal_id, 60279, char_id, ship_id, InventoryFlag::Cargo, 1, false));
    });
    ctx.invalidate_ship_fitting(ship_id);
    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "LoadAmmo", &[PyRep::Integer(ship_id), PyRep::List(vec![PyRep::Integer(strip_id)]), PyRep::List(vec![PyRep::Integer(crystal_id)])], &kw, None).await.expect("LoadAmmo crystal into Modulated Strip Miner II");
    ctx.advance_secs(5);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.items.values_mut().find(|i| i.location_id == ship_id && i.flag == InventoryFlag::HiSlot0 && i.type_id == 60279) {
            c.dynamic_attributes.insert(eve_sde::attr_ids::DAMAGE, 0.96);
        }
    });

    // Seed RNG so crit misses (>= 0.01), residue hits (< 0.54), and crystal volatility hits (< 0.075), shattering the crystal at 0 HP
    let hit_seed = (1u64..100_000)
        .find(|&s| {
            let mut st = s;
            let u0 = eve_server::rng::u64_to_unit_f64(eve_server::rng::splitmix64_next(&mut st));
            let u1 = eve_server::rng::u64_to_unit_f64(eve_server::rng::splitmix64_next(&mut st));
            let u2 = eve_server::rng::u64_to_unit_f64(eve_server::rng::splitmix64_next(&mut st));
            u0 >= 0.02 && u1 < 0.50 && u2 < 0.05
        })
        .expect("seed for residue + volatility hit");
    let before_rock = ctx.db.get_asteroid(big_rock_id).unwrap().quantity;
    ctx.set_mechanic_rng_seed(Some(hit_seed));
    let cry_cycle = crate::mining_cycle_engine::mine_once(&ctx, &mut sess, strip_id, big_rock_id)
        .await
        .expect("Modulated Strip Miner II + Type B I crystal cycle");
    assert_eq!(cry_cycle.units_delivered, 3_557, "Type B I crystal 1.5x yield on Modulated Strip Miner II (120 m³ base) delivers 3,557 Veldspar units");
    assert_eq!(cry_cycle.residue_units, 3_557, "Type B I crystal residue removes 3,557 extra units from asteroid");
    assert!(cry_cycle.crystal_damage > 0.0 && cry_cycle.crystal_shattered, "Crystal volatility roll damages and shatters crystal at 0 HP");
    assert!(ctx.db.with_snapshot(|s| !s.items.values().any(|i| i.location_id == ship_id && i.type_id == 60279)), "Shattered crystal must be destroyed and removed from the module slot");
    assert_eq!(ctx.db.get_asteroid(big_rock_id).unwrap().quantity, before_rock - cry_cycle.units_removed - cry_cycle.residue_units, "Asteroid loses both mined units and wasted residue units");
}

// ====================================================================
// Test 2: Ice & Gas, Drones, Survey Scan, Compression, Reprocessing (C-6, MN-08..12)
// ====================================================================
#[tokio::test]
async fn test_action_plan_c6_mn08_to_mn12_harvesting_drones_scan_compression_reprocessing() {
    // parity: plan:C-6, plan:MN-08, plan:MN-09, plan:MN-10, plan:MN-11, plan:MN-12, notify:OnReprocessingMaterialsChanged
    eve_testkit::attest(&["plan:C-6", "plan:MN-08", "plan:MN-09", "plan:MN-10", "plan:MN-11", "plan:MN-12", "notify:OnReprocessingMaterialsChanged"]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let kw = HashMap::new();
    let char_id = 90_000_001i64;
    let mut sess = setup_pilot_session(&ctx, char_id).await;

    let undock = ctx.dispatch_rpc(&mut sess, "stationSvc", None, "Undock", &[], &kw, None).await.expect("Undock");
    let ship_id = get_dict_val(&undock, "shipID").unwrap().as_i64().unwrap();
    let jita = ctx.systems.get_or_spawn(30000142);
    let (_, ship_ball) = jita.get_ball_snapshot(ship_id).await.unwrap().unwrap();

    ctx.db.with_mut_snapshot(|snap| {
        let sk = snap.skills.entry(char_id).or_default();
        for (sid, lvl) in [
            (3386, 5), (3410, 5), (17940, 5), (16281, 5), (25544, 5), (3436, 5), (3438, 1), (3442, 1),
            (3385, 5), (3389, 5), (18025, 5), (12195, 5), (18023, 5), (18020, 5), (60377, 5), (12196, 5),
        ] {
            sk.insert(sid, TrainedSkillRecord { type_id: sid, level: lvl, skill_points: 256_000.0 });
        }
    });

    // 1. plan:MN-08: Execute actual Ice Harvester I cycle (1 block = 1,000 m³) and Gas Cloud Harvester I cycle
    let (ice_ast_id, gas_cloud_id, veld_ast_id, far_ast_id) = (750_000_601i64, 750_000_602i64, 750_000_603i64, 750_000_604i64);
    for (id, tid, qty, dx) in [
        (ice_ast_id, 16262u32, 50i64, 1000.0),
        (gas_cloud_id, 30370u32, 500i64, 1200.0),
        (veld_ast_id, 1230u32, 50_000i64, 800.0),
        (far_ast_id, 1230u32, 10_000i64, 75_000.0),
    ] {
        ctx.db.spawn_asteroid(AsteroidRecord {
            asteroid_id: id, solar_system_id: 30000142, belt_id: 40_000_001, type_id: tid,
            quantity: qty, initial_quantity: qty, position: (ship_ball.position.x + dx, ship_ball.position.y, ship_ball.position.z), radius: 80.0,
        });
    }

    let ice_harv_id = 800_000_600i64;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ship) = snap.items.get_mut(&ship_id) {
            ship.type_id = 17478; // Retriever
        }
        snap.items.retain(|_, i| i.location_id != ship_id);
        snap.items.insert(ice_harv_id, make_item(ice_harv_id, 16278, char_id, ship_id, InventoryFlag::HiSlot0, 1, true));
    });
    ctx.invalidate_ship_fitting(ship_id);

    ctx.set_mechanic_rng_seed(Some(crate::mining_cycle_engine::seed_for(&[false], 0.01)));
    let ice_cycle = crate::mining_cycle_engine::mine_once(&ctx, &mut sess, ice_harv_id, ice_ast_id)
        .await
        .expect("Ice Harvester I cycle");
    assert_eq!((ice_cycle.mining_amount_m3, ice_cycle.units_removed, ice_cycle.units_delivered), (1_000.0, 1, 1));
    assert_eq!(ctx.db.get_asteroid(ice_ast_id).unwrap().quantity, 49, "Ice asteroid decrements by 1 block");

    let gas_harv_id = 800_000_601i64;
    let scanner_item_id = 800_000_602i64;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ship) = snap.items.get_mut(&ship_id) {
            ship.type_id = 32880; // Venture
        }
        snap.items.retain(|_, i| i.location_id != ship_id);
        snap.items.insert(gas_harv_id, make_item(gas_harv_id, 25542, char_id, ship_id, InventoryFlag::HiSlot0, 1, true));
        snap.items.insert(scanner_item_id, make_item(scanner_item_id, 444, char_id, ship_id, InventoryFlag::MedSlot0, 1, true));
    });
    ctx.invalidate_ship_fitting(ship_id);

    let gas_cycle = crate::mining_cycle_engine::mine_once(&ctx, &mut sess, gas_harv_id, gas_cloud_id)
        .await
        .expect("Gas Cloud Harvester I cycle");
    assert!(gas_cycle.units_delivered > 0, "Gas Cloud Harvester delivers Fullerite-C50 gas to hold");
    assert_eq!(ctx.db.get_asteroid(gas_cloud_id).unwrap().quantity, 500 - gas_cycle.units_removed - gas_cycle.residue_units);

    // 2. plan:MN-09 & plan:MN-10: Survey Scanner before mining -> 2 Mining Drone Is cycle & stop when hold full -> Survey Scanner after mining
    let scan_before = ctx
        .dispatch_rpc(&mut sess, "miningScanMgr", None, "perform_scan", &[PyRep::Integer(scanner_item_id)], &kw, None)
        .await
        .expect("perform_scan before drone mining");
    let PyRep::List(asts_before) = get_dict_val(&scan_before, "asteroids").unwrap() else { panic!() };
    let veld_before_qty = asts_before
        .iter()
        .find(|e| get_dict_val(e, "asteroidID").and_then(|v| v.as_i64()) == Some(veld_ast_id))
        .and_then(|e| get_dict_val(e, "quantity"))
        .and_then(|v| v.as_i64())
        .expect("veld_ast_id in perform_scan before mining");
    assert_eq!(veld_before_qty, 50_000);

    // Launch two Mining Drone Is (10246, 25 m³ base, 5 mbit each = 10 mbit in Venture's 10 mbit bandwidth)
    // Pre-fill Venture SpecializedOreHold (5,000 m³ = 50,000 Veldspar) with 49,000 Veldspar so 1st cycle (2 * 393 = 786) fits (49,786) and 2nd cycle stops both drones
    let drone_a = 800_000_605i64;
    let drone_b = 800_000_606i64;
    let prefill_ore_id = 800_000_607i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.retain(|_, i| !(i.location_id == ship_id && i.flag == InventoryFlag::SpecializedOreHold));
        snap.items.insert(prefill_ore_id, make_item(prefill_ore_id, 1230, char_id, ship_id, InventoryFlag::SpecializedOreHold, 49_000, false));
        snap.items.insert(drone_a, make_item(drone_a, 10246, char_id, ship_id, InventoryFlag::DroneBay, 1, true));
        snap.items.insert(drone_b, make_item(drone_b, 10246, char_id, ship_id, InventoryFlag::DroneBay, 1, true));
    });
    let drone_pair = PyRep::List(vec![PyRep::Integer(drone_a), PyRep::Integer(drone_b)]);
    ctx.dispatch_rpc(&mut sess, "ship", None, "LaunchDrones", std::slice::from_ref(&drone_pair), &kw, None)
        .await
        .expect("ship.LaunchDrones 2 Mining Drone Is");

    // Range check: asteroid at 75 km (> 60 km drone control range) is rejected with OutOfRange
    let range_err = ctx
        .dispatch_rpc(&mut sess, "entity", None, "CmdMineRepeatedly", &[drone_pair.clone(), PyRep::Integer(far_ast_id)], &kw, None)
        .await
        .expect_err("CmdMineRepeatedly beyond drone control range must fail");
    assert_eq!(range_err.code, "OutOfRange");

    ctx.dispatch_rpc(&mut sess, "entity", None, "CmdMineRepeatedly", &[drone_pair, PyRep::Integer(veld_ast_id)], &kw, None)
        .await
        .expect("entity.CmdMineRepeatedly 2 Mining Drone Is");
    let c1_dur = eve_server::services::combat_crimewatch::with_combat_runtime(&ctx, |rt| {
        assert_eq!(rt.active_drone_miners.len(), 2, "Both Mining Drone Is are actively mining");
        rt.active_drone_miners.values().next().unwrap().duration_secs
    });
    assert!(c1_dur > 60, "Mining Drone I cycle duration ({c1_dur}s) includes round-trip travel time above 60s base");

    // Cycle 1: Each Mining Drone I yields 25 * 1.05 * 1.50 = 39.375 m³ -> 393 Veldspar units (786 total across both drones)
    ctx.advance_secs(c1_dur);
    eve_server::sp6_lifecycle::step_repeating_drone_mining(&ctx, ctx.now_unix()).await;
    let hold_after_c1: i32 = ctx.db.with_snapshot(|snap| {
        snap.items
            .values()
            .filter(|i| i.location_id == ship_id && i.flag == InventoryFlag::SpecializedOreHold && i.type_id == 1230)
            .map(|i| i.quantity)
            .sum()
    });
    assert_eq!(hold_after_c1, 49_000 + 786, "2 Mining Drone Is deposit 786 Veldspar units into SpecializedOreHold (49,786 total)");

    let scan_after = ctx
        .dispatch_rpc(&mut sess, "miningScanMgr", None, "perform_scan", &[PyRep::Integer(scanner_item_id)], &kw, None)
        .await
        .expect("perform_scan after drone mining");
    let PyRep::List(asts_after) = get_dict_val(&scan_after, "asteroids").unwrap() else { panic!() };
    let veld_after_qty = asts_after
        .iter()
        .find(|e| get_dict_val(e, "asteroidID").and_then(|v| v.as_i64()) == Some(veld_ast_id))
        .and_then(|e| get_dict_val(e, "quantity"))
        .and_then(|v| v.as_i64())
        .expect("veld_ast_id in perform_scan after mining");
    assert_eq!(veld_after_qty, 50_000 - 786, "Survey scan quantity after mining matches depleted asteroid quantity (49,214)");

    // Cycle 2: Only 21.4 m³ (214 units) left in SpecializedOreHold (< 39.375 m³ per drone) -> both Mining Drone Is stop and return to idle without depleting asteroid!
    ctx.advance_secs(c1_dur);
    eve_server::sp6_lifecycle::step_repeating_drone_mining(&ctx, ctx.now_unix()).await;
    let (active_after_full, idle_a, idle_b) = eve_server::services::combat_crimewatch::with_combat_runtime(&ctx, |rt| {
        let a = rt.active_drones_in_space.values().find(|d| d.drone_item_id == drone_a || d.ball_id == drone_a).map(|d| d.activity_state);
        let b = rt.active_drones_in_space.values().find(|d| d.drone_item_id == drone_b || d.ball_id == drone_b).map(|d| d.activity_state);
        (rt.active_drone_miners.len(), a, b)
    });
    assert_eq!(active_after_full, 0, "Both Mining Drone Is stop mining when SpecializedOreHold is full");
    assert_eq!((idle_a, idle_b), (Some(0), Some(0)), "Both Mining Drone Is return to idle activity_state == 0");
    assert_eq!(ctx.db.get_asteroid(veld_ast_id).unwrap().quantity, 49_214, "Asteroid quantity must be untouched when drone stops due to full hold");

    // Exercise 'Excavator' Mining Drone (41030) + Drone Mining Augmentor I (25918, +10% rig) and 'Excavator' Ice Harvesting Drone (43681) with Ice Harvesting Drone Operation V (43702) + Specialization V (43703) on Rorqual (28352) + Capital Industrial Core I (28583)
    let (excavator_id, ice_drone_id, rorq_core_id, rig_id) = (800_000_609i64, 800_000_610i64, 800_000_650i64, 800_000_651i64);
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&ship_id).unwrap().type_id = 28352;
        snap.items.retain(|_, i| i.location_id != ship_id);
        snap.items.insert(excavator_id, make_item(excavator_id, 41030, char_id, ship_id, InventoryFlag::DroneBay, 1, true));
        snap.items.insert(ice_drone_id, make_item(ice_drone_id, 43681, char_id, ship_id, InventoryFlag::DroneBay, 1, true));
        snap.items.insert(rorq_core_id, make_item(rorq_core_id, 28583, char_id, ship_id, InventoryFlag::HiSlot0, 1, true));
        snap.items.insert(rig_id, make_item(rig_id, 25918, char_id, ship_id, InventoryFlag::RigSlot0, 1, true));
        for sid in [22541u32, 28374, 43702, 43703] { snap.skills.get_mut(&char_id).unwrap().insert(sid, TrainedSkillRecord { type_id: sid, level: 5, skill_points: 256_000.0 }); }
    });
    ctx.invalidate_ship_fitting(ship_id);
    eve_server::services::combat_crimewatch::with_mut_combat_runtime(&ctx, |rt| rt.active_drones_in_space.clear());
    ctx.dispatch_rpc(&mut sess, "ship", None, "LaunchDrones", &[PyRep::List(vec![PyRep::Integer(excavator_id), PyRep::Integer(ice_drone_id)])], &kw, None).await.expect("Launch Excavator + Ice Excavator");
    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(rorq_core_id), PyRep::String("industrialCore".into())], &kw, None).await.expect("Activate Capital Industrial Core I");
    ctx.dispatch_rpc(&mut sess, "entity", None, "CmdMineRepeatedly", &[PyRep::List(vec![PyRep::Integer(excavator_id)]), PyRep::Integer(veld_ast_id)], &kw, None).await.expect("CmdMineRepeatedly Excavator");
    ctx.dispatch_rpc(&mut sess, "entity", None, "CmdMineRepeatedly", &[PyRep::List(vec![PyRep::Integer(ice_drone_id)]), PyRep::Integer(ice_ast_id)], &kw, None).await.expect("CmdMineRepeatedly Ice Excavator");
    let ice_dur = eve_server::services::combat_crimewatch::with_combat_runtime(&ctx, |rt| rt.active_drone_miners.values().find(|m| m.ore_type_id == 16262).map(|m| m.duration_secs).unwrap());
    assert!(ice_dur < 100, "'Excavator' Ice Harvesting Drone 300s base cycle is reduced by 43702 V, 43703 V, 28374 V, and Core I to {ice_dur}s");
    ctx.advance_secs(120);
    eve_server::sp6_lifecycle::step_repeating_drone_mining(&ctx, ctx.now_unix()).await;
    assert_eq!(ctx.db.with_snapshot(|snap| snap.items.values().filter(|i| i.location_id == ship_id && i.flag == InventoryFlag::SpecializedOreHold && i.type_id == 1230).map(|i| i.quantity).sum::<i32>()), 6_174, "'Excavator' Mining Drone + Drone Mining Augmentor I (+10%) deposits 6,174 Veldspar units");
    assert_eq!(ctx.db.with_snapshot(|snap| snap.items.values().filter(|i| i.location_id == ship_id && i.flag == InventoryFlag::SpecializedOreHold && i.type_id == 16262).map(|i| i.quantity).sum::<i32>()), 4, "'Excavator' Ice Harvesting Drone deposits 4 Clear Icicle units into SpecializedOreHold");

    // 3. plan:MN-11 & plan:C-6: V24 1:1 lossless compression in-space (Orca + Core + Compressor) and in-structure + gas decompression
    let space_ore_id = 800_000_608i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.retain(|_, i| !(i.location_id == ship_id && i.flag == InventoryFlag::SpecializedOreHold));
        snap.items.insert(space_ore_id, make_item(space_ore_id, 1230, char_id, ship_id, InventoryFlag::SpecializedOreHold, 1_234, false));
    });
    let no_comp_err = ctx.dispatch_rpc(&mut sess, "inSpaceCompressionMgr", None, "CompressItemInSpace", &[PyRep::Integer(space_ore_id)], &kw, None).await.expect_err("CompressItemInSpace without active compressor must fail");
    assert_eq!(no_comp_err.code, "NoActiveCompressorInRange");

    let (core_mod_id, comp_mod_id, hw_id) = (800_000_651i64, 800_000_652i64, 800_000_653i64);
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&ship_id).unwrap().type_id = 28606; // Orca
        snap.items.retain(|_, i| i.location_id != ship_id || i.flag == InventoryFlag::SpecializedOreHold);
        snap.items.insert(core_mod_id, make_item(core_mod_id, 58945, char_id, ship_id, InventoryFlag::HiSlot0, 1, true));
        snap.items.insert(comp_mod_id, make_item(comp_mod_id, 62625, char_id, ship_id, InventoryFlag::HiSlot1, 1, true));
        snap.items.insert(hw_id, make_item(hw_id, 16272, char_id, ship_id, InventoryFlag::Cargo, 5_000, false));
    });
    ctx.invalidate_ship_fitting(ship_id);
    assert_eq!(ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(comp_mod_id), PyRep::String("compressor".into())], &kw, None).await.unwrap_err().code, "IndustrialCoreNotActive");

    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(core_mod_id), PyRep::String("industrialCore".into())], &kw, None).await.expect("Activate Core");
    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "Activate", &[PyRep::Integer(comp_mod_id), PyRep::String("compressor".into())], &kw, None).await.expect("Activate Compressor");
    let space_comp_res = ctx.dispatch_rpc(&mut sess, "inSpaceCompressionMgr", None, "CompressItemInSpace", &[PyRep::Integer(space_ore_id)], &kw, None).await.expect("CompressItemInSpace");
    assert_eq!((get_dict_val(&space_comp_res, "compressedTypeID").and_then(|v| v.as_i64()), get_dict_val(&space_comp_res, "compressedUnits").and_then(|v| v.as_i64()), get_dict_val(&space_comp_res, "remainderUnits").and_then(|v| v.as_i64())), (Some(62516), Some(1_234), Some(0)));
    assert_eq!(ctx.db.with_snapshot(|snap| (snap.items[&space_ore_id].type_id, snap.items[&space_ore_id].quantity)), (62516, 1_234));

    let (comp_structure_id, reproc_svc_mod, rig_mod, uncompressed_ore_id, comp_gas_id) = (1_000_000_001_020i64, 800_000_610i64, 800_000_614i64, 800_000_611i64, 800_000_615i64);
    let comp_c50_tid = ctx.sde.compressible_types[&30370];
    let char_corp_id = ctx.db.with_snapshot(|snap| snap.characters[&char_id].corporation_id);
    ctx.db.with_mut_snapshot(|snap| {
        let corp = snap.corporations.entry(char_corp_id).or_insert_with(|| eve_db::CorporationRecord {
            corporation_id: char_corp_id, name: "Test Corp".into(), ticker: "TC".into(), ceo_id: char_id, alliance_id: None,
            tax_rate: 0.15, master_wallet: eve_db::IskCents(0), wallet_divisions: Vec::new(), description: String::new(),
            url: String::new(), hq_station_id: Some(60003760), members: Default::default(), member_roles: Default::default(), applications: Default::default(),
        });
        corp.tax_rate = 0.15; // 15% corp member income tax (must NOT be used as structure reprocessing tax)
        corp.ceo_id = char_id;
        snap.structures.insert(comp_structure_id, make_structure(comp_structure_id, char_corp_id, 35836, 30000142, "Rigged Tatara", [0.0, 0.0, 0.0]));
        snap.items.insert(reproc_svc_mod, make_item(reproc_svc_mod, 35892, char_corp_id, comp_structure_id, InventoryFlag::ServiceSlot0, 1, true));
        snap.items.insert(rig_mod, make_item(rig_mod, 46640, char_corp_id, comp_structure_id, InventoryFlag::RigSlot0, 1, true));
        snap.items.insert(uncompressed_ore_id, make_item(uncompressed_ore_id, 1230, char_id, comp_structure_id, InventoryFlag::Hangar, 1_234, false));
        snap.items.insert(comp_gas_id, make_item(comp_gas_id, comp_c50_tid, char_id, comp_structure_id, InventoryFlag::Hangar, 100, false));
        let ch = snap.characters.get_mut(&char_id).unwrap(); ch.station_id = None; ch.structure_id = Some(comp_structure_id);
    });
    sess.station_id = None; sess.structure_id = Some(comp_structure_id);
    ctx.dispatch_rpc(&mut sess, "structureSettings", None, "SaveProfileSettings", &[PyRep::Integer(comp_structure_id), PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(3), PyRep::Real(0.04)])])], &kw, None).await.expect("SaveProfileSettings 4% reprocessing service tax");
    let comp_res = ctx.dispatch_rpc(&mut sess, "structureCompressionMgr", None, "CompressItemInStructure", &[PyRep::Integer(uncompressed_ore_id)], &kw, None).await.expect("CompressItemInStructure");
    assert_eq!((get_dict_val(&comp_res, "compressedTypeID").and_then(|v| v.as_i64()), get_dict_val(&comp_res, "compressedUnits").and_then(|v| v.as_i64()), get_dict_val(&comp_res, "remainderUnits").and_then(|v| v.as_i64())), (Some(62516), Some(1_234), Some(0)));
    let eff = ctx.dispatch_rpc(&mut sess, "structureCompressionMgr", None, "GetMyGasDecompressionEfficiency", &[PyRep::Integer(comp_structure_id)], &kw, None).await.expect("GetMyGasDecompressionEfficiency").as_f64().unwrap();
    let decomp_res = ctx.dispatch_rpc(&mut sess, "structureCompressionMgr", None, "DecompressGasInStructure", &[PyRep::Integer(comp_gas_id)], &kw, None).await.expect("DecompressGasInStructure");
    assert_eq!((get_dict_val(&decomp_res, "decompressedTypeID").and_then(|v| v.as_i64()), get_dict_val(&decomp_res, "decompressedUnits").and_then(|v| v.as_i64())), (Some(30370), Some((100.0 * eff).floor() as i64)));

    // Rigged Tatara reprocessing of 1,000 Veldspar -> 0.53 * 1.055 * 1.15 * 1.10 * 1.10 = 0.77820125 yield (3,112 Tritanium) > station yield (2,783), and 4% structure service tax (NOT 15% corp.tax_rate)
    let tatara_veld_id = 800_000_616i64;
    ctx.db.with_mut_snapshot(|snap| { snap.items.insert(tatara_veld_id, make_item(tatara_veld_id, 1230, char_id, comp_structure_id, InventoryFlag::Hangar, 1_000, false)); });
    let tatara_rep = ctx.dispatch_rpc(&mut sess, "reprocessingSvc", None, "Reprocess", &[PyRep::Integer(tatara_veld_id)], &kw, None).await.expect("Reprocess in rigged Tatara");
    assert_eq!(get_dict_val(&tatara_rep, "tritaniumQuantity").and_then(|v| v.as_i64()), Some(3_112));
    assert!((get_dict_val(&tatara_rep, "stationTaxRate").unwrap().as_f64().unwrap() - 0.04).abs() < 1e-6, "Structure reprocessing tax must use per-structure service setting (4%), not corp.tax_rate (15%)");

    // 4. plan:MN-12 & plan:C-6: Station reprocessing (V24 GetQuotes 3-tuple, 5-arg Reprocess list, implant 27174, tax standing curve, Scrapmetal & rejection)
    let (station_id, reprocess_item_id, scordite_item_id, pyrox_item_id, veld_imp_id, scrap_mod_id, non_reprocess_item) =
        (60003760i64, 800_000_612i64, 800_000_617i64, 800_000_618i64, 800_000_620i64, 800_000_619i64, 800_000_613i64);
    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&char_id).unwrap();
        ch.station_id = Some(station_id);
        ch.structure_id = None;
        ch.standings.insert(1000035, 2.0);
        snap.items.insert(reprocess_item_id, make_item(reprocess_item_id, 1230, char_id, station_id, InventoryFlag::Hangar, 1_050, false));
        snap.items.insert(scordite_item_id, make_item(scordite_item_id, 1228, char_id, station_id, InventoryFlag::Hangar, 1_000, false));
        snap.items.insert(pyrox_item_id, make_item(pyrox_item_id, 1224, char_id, station_id, InventoryFlag::Hangar, 1_000, false));
        snap.items.insert(veld_imp_id, make_item(veld_imp_id, 1230, char_id, station_id, InventoryFlag::Hangar, 1_000, false));
        snap.items.insert(scrap_mod_id, make_item(scrap_mod_id, 483, char_id, station_id, InventoryFlag::Hangar, 1, false));
        snap.items.insert(non_reprocess_item, make_item(non_reprocess_item, 34, char_id, station_id, InventoryFlag::Hangar, 1_000, false));
    });
    sess.structure_id = None;
    sess.station_id = Some(station_id);
    let expected_yield = 0.50 * 1.15 * 1.10 * 1.10; // 0.69575
    let expected_tax = 0.05 - 0.0075 * 2.0; // 0.035
    let expected_trit = (10.0_f64 * 400.0 * expected_yield).floor() as i64; // 2,783 Tritanium
    let PyRep::Tuple(quotes_tup) = ctx.dispatch_rpc(&mut sess, "reprocessingSvc", None, "GetQuotes", &[PyRep::List(vec![PyRep::Integer(reprocess_item_id)]), PyRep::Integer(ship_id)], &kw, None).await.expect("2-arg GetQuotes") else { panic!("Expected 3-tuple (stationTax, stationEfficiency, rawQuotes)") };
    assert!((quotes_tup[0].as_f64().unwrap() - expected_tax).abs() < 1e-6 && (quotes_tup[1].as_f64().unwrap() - 0.50).abs() < 1e-6);
    let PyRep::List(raw_quotes) = &quotes_tup[2] else { panic!() };
    assert_eq!(raw_quotes.len(), 1);
    ctx.drain_notifications_for_char(char_id);
    let reprocess_res = ctx.dispatch_rpc(&mut sess, "reprocessingSvc", None, "Reprocess", &[PyRep::Integer(reprocess_item_id)], &kw, None).await.expect("Reprocess 1,050 Veldspar");
    assert!((get_dict_val(&reprocess_res, "yieldRate").unwrap().as_f64().unwrap() - expected_yield).abs() < 1e-6 && (get_dict_val(&reprocess_res, "stationTaxRate").unwrap().as_f64().unwrap() - expected_tax).abs() < 1e-6);
    assert_eq!(get_dict_val(&reprocess_res, "tritaniumQuantity").and_then(|v| v.as_i64()), Some(expected_trit));
    assert!(ctx.drain_notifications_for_char(char_id).iter().any(|n| n.event == "OnReprocessingMaterialsChanged" && n.args.is_empty()), "OnReprocessingMaterialsChanged (arity 0) must fire");

    // Standing >= 6.67 eliminates station tax (0%) and 5-arg batch Reprocess(itemIDs, fromLocation, ownerID, flag, skipChecks) returns List for Scordite + Pyroxeres
    ctx.db.with_mut_snapshot(|snap| { snap.characters.get_mut(&char_id).unwrap().standings.insert(1000035, 6.67); });
    let PyRep::List(batch_res) = ctx.dispatch_rpc(&mut sess, "reprocessingSvc", None, "Reprocess", &[PyRep::List(vec![PyRep::Integer(scordite_item_id), PyRep::Integer(pyrox_item_id)]), PyRep::Integer(station_id), PyRep::Integer(char_id), PyRep::Integer(4), PyRep::List(vec![])], &kw, None).await.expect("5-arg Reprocess Scordite + Pyroxeres") else { panic!("Expected List from 5-arg Reprocess") };
    assert_eq!(batch_res.len(), 2);
    assert_eq!(get_dict_val(&batch_res[0], "stationTaxRate").and_then(|v| v.as_f64()), Some(0.0));
    for (idx, tid) in [(0usize, 1228u32), (1usize, 1224u32)] {
        let PyRep::List(mats) = get_dict_val(&batch_res[idx], "outputs").unwrap() else { panic!() };
        for mat in &ctx.sde.type_materials[&tid] {
            let exp = ((10 * mat.quantity) as f64 * expected_yield).floor() as i64;
            assert!(mats.iter().any(|m| get_dict_val(m, "typeID").and_then(|v| v.as_i64()) == Some(mat.material_type_id as i64) && get_dict_val(m, "quantity").and_then(|v| v.as_i64()) == Some(exp)));
        }
    }

    // Implant factor: Zainou 'Beancounter' Reprocessing RX-804 (27174, +4% attr 379) -> 0.69575 * 1.04 = 0.72358 (2,894 Tritanium)
    ctx.db.with_mut_snapshot(|snap| { snap.active_implants.entry(char_id).or_default().push(27174); });
    let imp_res = ctx.dispatch_rpc(&mut sess, "reprocessingSvc", None, "Reprocess", &[PyRep::Integer(veld_imp_id)], &kw, None).await.expect("Reprocess with RX-804 implant");
    assert_eq!(get_dict_val(&imp_res, "tritaniumQuantity").and_then(|v| v.as_i64()), Some((10.0 * 400.0 * expected_yield * 1.04).floor() as i64));

    let scrap_res = ctx.dispatch_rpc(&mut sess, "reprocessingSvc", None, "Reprocess", &[PyRep::Integer(scrap_mod_id)], &kw, None).await.expect("Reprocess Miner I module with Scrapmetal Processing V");
    assert!((get_dict_val(&scrap_res, "yieldRate").unwrap().as_f64().unwrap() - 0.55).abs() < 1e-6, "Scrapmetal Processing V yields 0.50 * 1.10 = 0.55");

    let (rem_veld, has_tax_journal) = ctx.db.with_snapshot(|snap| {
        let rem = snap.items.get(&reprocess_item_id).map(|i| i.quantity).unwrap_or(0);
        let journaled = snap.wallet_journal.iter().any(|e| e.character_id == char_id && e.entry_type == "ReprocessingTax" && e.amount.0 > 0);
        (rem, journaled)
    });
    assert_eq!(rem_veld, 50, "Unreprocessed 50-unit partial batch (< 100 portionSize) remains in hangar");
    assert!(has_tax_journal, "Station reprocessing tax is recorded in the wallet journal");
    let reject_err = ctx.dispatch_rpc(&mut sess, "reprocessingSvc", None, "Reprocess", &[PyRep::Integer(non_reprocess_item)], &kw, None).await.expect_err("Raw mineral must be rejected");
    assert_eq!(reject_err.code, "ItemNotReprocessable");
}
