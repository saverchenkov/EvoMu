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

    // plan:MN-07 implants (Highwall MX-1005 22535 +5% & Michi's Excavation Augmentor 20700 +5%) and Mining Foreman Burst II (43551) charges (42830 Optimization, 42829 Field Enhancement) from fleetmate Porpoise (42244)
    let cmd_proc_tid = ctx
        .sde
        .types
        .iter()
        .find_map(|(&tid, t)| {
            (t.effect_ids.contains(&6766) && t.default_attributes.get(&1547) == Some(&2.0)).then_some(tid)
        })
        .unwrap();
    let (burst_opt, burst_fld, booster_cid, booster_ship) = (800_000_524i64, 800_000_527i64, 800_000_528i64, 800_000_529i64);
    ctx.db.with_mut_snapshot(|snap| {
        snap.active_implants.entry(char_id).or_default().extend([22_535, 20_700]);
        let sk = snap.skills.entry(char_id).or_default();
        for sid in [3_348u32, 3_426, 3_413, 22_536, 22_552] { sk.insert(sid, TrainedSkillRecord { type_id: sid, level: 5, skill_points: 256_000.0 }); }
        let mut b_char = snap.characters[&char_id].clone();
        b_char.character_id = booster_cid;
        b_char.name = "Booster".into();
        b_char.active_ship_id = booster_ship;
        b_char.station_id = None;
        b_char.solar_system_id = 30_000_142;
        snap.characters.insert(booster_cid, b_char);
        snap.skills.insert(booster_cid, snap.skills[&char_id].clone());
        snap.items.insert(booster_ship, make_item(booster_ship, 22_446, booster_cid, 30_000_142, InventoryFlag::None, 1, true));
        snap.items.insert(800_000_531, make_item(800_000_531, 1_229, booster_cid, booster_ship, InventoryFlag::LoSlot0, 1, true));
        snap.items.insert(800_000_532, make_item(800_000_532, cmd_proc_tid, booster_cid, booster_ship, InventoryFlag::RigSlot0, 1, true));
        snap.items.insert(800_000_533, make_item(800_000_533, cmd_proc_tid, booster_cid, booster_ship, InventoryFlag::RigSlot1, 1, true));
        snap.items.insert(burst_opt, make_item(burst_opt, 43_551, booster_cid, booster_ship, InventoryFlag::HiSlot0, 1, true));
        snap.items.insert(800_000_525, make_item(800_000_525, 42_830, booster_cid, booster_ship, InventoryFlag::HiSlot0, 10, false));
        snap.items.insert(burst_fld, make_item(burst_fld, 43_551, booster_cid, booster_ship, InventoryFlag::HiSlot1, 1, true));
        snap.items.insert(800_000_526, make_item(800_000_526, 42_829, booster_cid, booster_ship, InventoryFlag::HiSlot1, 10, false));
        snap.fleets.insert(800_000_530, eve_db::FleetRecord { fleet_id: 800_000_530, boss_id: booster_cid, members: vec![booster_cid, char_id], ..Default::default() });
    });
    jita.spawn_ball(Ball { id: booster_ship, type_id: 22_446, position: rock_pos + DVec3::new(1000.0, 0.0, 0.0), velocity: DVec3::ZERO, max_velocity: 150.0, inertia: 1.0, mass: 20e6, radius: 150.0, speed_fraction: 0.0, mode: MovementMode::Stop }).await.unwrap();
    ctx.invalidate_ship_fitting(booster_ship);
    let mut booster_sess = sess.clone();
    booster_sess.character_id = Some(booster_cid);
    booster_sess.ship_id = Some(booster_ship);
    let _ = ctx.dispatch_rpc(&mut booster_sess, "dogmaIM", None, "SetModuleOnline", &[PyRep::Integer(booster_ship), PyRep::Integer(burst_fld)], &kw, None).await;
    ctx.dispatch_rpc(&mut booster_sess, "dogmaIM", None, "Activate", &[PyRep::Integer(burst_opt), PyRep::String("moduleBonusWarfareLinkMining".into())], &kw, None).await.expect("Activate Optimization Burst");
    ctx.dispatch_rpc(&mut booster_sess, "dogmaIM", None, "Activate", &[PyRep::Integer(burst_fld), PyRep::String("moduleBonusWarfareLinkMining".into())], &kw, None).await.expect("Activate Field Burst");
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
    eve_server::session_cluster_lifecycle::step_repeating_drone_mining(&ctx, ctx.now_unix()).await;
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
    eve_server::session_cluster_lifecycle::step_repeating_drone_mining(&ctx, ctx.now_unix()).await;
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
    eve_server::session_cluster_lifecycle::step_repeating_drone_mining(&ctx, ctx.now_unix()).await;
    assert_eq!(ctx.db.with_snapshot(|snap| snap.items.values().filter(|i| i.location_id == ship_id && i.flag == InventoryFlag::SpecializedOreHold && i.type_id == 1230).map(|i| i.quantity).sum::<i32>()), 6_174, "'Excavator' Mining Drone + Drone Mining Augmentor I (+10%) deposits 6,174 Veldspar units");
    assert_eq!(ctx.db.with_snapshot(|snap| snap.items.values().filter(|i| i.location_id == ship_id && i.flag == InventoryFlag::SpecializedOreHold && i.type_id == 16262).map(|i| i.quantity).sum::<i32>()), 1, "'Excavator' Ice Harvesting Drone deposits 1 Clear Icicle unit into SpecializedOreHold");

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

pub(crate) mod to_moons_metenox_sites_ledgers {
    // Acceptance test suite for Advanced Mining, Moons, Sites & Ledgers (`Phase 5M`: `MN-13`..`MN-16`).
    //
    // Validates canonical tasks `plan:MN-13`, `plan:MN-14`, `plan:MN-15`, `plan:MN-16`,
    // and `notify:OnMoonExtractionUpdate` against the authoritative "Done when" criteria in `action_plan.md`.


    use crate::mining_operations_and_ledgers::{get_dict_val, make_item, make_structure, setup_pilot_session};
    use crate::common;
    use eve_db::{AsteroidRecord, InventoryFlag, TrainedSkillRecord};
    use eve_destiny::{Ball, MovementMode};
    use eve_proto::PyRep;
    use eve_server::router::ServerContext;
    use eve_server::services::mining_sites;
    use glam::DVec3;
    use std::collections::HashMap;

    fn decode_crowset_rows(rep: &PyRep) -> Vec<Vec<PyRep>> {
        if let PyRep::ObjectEx { list, .. } = rep {
            return list
                .iter()
                .filter_map(|r| match r {
                    PyRep::DBRow { columns, .. } => {
                        Some(columns.iter().map(|(_, v)| v.clone()).collect())
                    }
                    PyRep::List(values) | PyRep::Tuple(values) => Some(values.clone()),
                    _ => None,
                })
                .collect();
        }
        Vec::new()
    }

    #[tokio::test]
    async fn test_action_plan_mn13_to_mn16_moons_metenox_sites_ledgers() {
        // parity: plan:MN-13, plan:MN-14, plan:MN-15, plan:MN-16, notify:OnMoonExtractionUpdate
        eve_testkit::attest(&[
            "plan:MN-13",
            "plan:MN-14",
            "plan:MN-15",
            "plan:MN-16",
            "notify:OnMoonExtractionUpdate",
        ]);

        let ctx = ServerContext::new();
        common::pin_clock(&ctx);
        eve_server::services::serverops_cluster::set_scheduled_downtime_enabled(&ctx, false);
        let kw = HashMap::new();
        let char_id = 90_000_001i64;
        let mut sess = setup_pilot_session(&ctx, char_id).await;

        let undock = ctx.dispatch_rpc(&mut sess, "stationSvc", None, "Undock", &[], &kw, None).await.expect("Undock");
        let ship_id = get_dict_val(&undock, "shipID").unwrap().as_i64().unwrap();
        let jita = ctx.systems.get_or_spawn(30000142);

        // 1. plan:MN-13 & notify:OnMoonExtractionUpdate: Moon extraction start, cancel, fracture, ore mix, jackpot & corp ledger
        let moon_structure_id = 1_000_000_001_030i64;
        let jackpot_structure_id = 1_000_000_001_032i64;
        let metenox_id = 1_000_000_001_031i64;
        let char_corp_id = ctx.db.with_snapshot(|snap| snap.characters[&char_id].corporation_id);

        ctx.db.with_mut_snapshot(|snap| {
            let sk = snap.skills.entry(char_id).or_default();
            for sid in [3386u32, 3410, 17940, 32918, 3429] {
                sk.insert(sid, TrainedSkillRecord { type_id: sid, level: 5, skill_points: 256_000.0 });
            }
            let corp = snap.corporations.entry(char_corp_id).or_insert_with(|| eve_db::CorporationRecord {
                corporation_id: char_corp_id,
                name: "Test Mining Corp".into(),
                ticker: "TMC".into(),
                ceo_id: char_id,
                alliance_id: None,
                tax_rate: 0.0,
                master_wallet: eve_db::IskCents(0),
                wallet_divisions: Vec::new(),
                description: String::new(),
                url: String::new(),
                hq_station_id: Some(60003760),
                members: Default::default(),
                member_roles: Default::default(),
                applications: Default::default(),
            });
            corp.ceo_id = char_id;
            corp.member_roles.insert(char_id, eve_db::corp_roles::ROLE_DIRECTOR);
            snap.structures.insert(
                moon_structure_id,
                make_structure(moon_structure_id, char_corp_id, 35835, 30000142, "Moon Athanor", [1000.0, 2000.0, 3000.0]),
            );
            snap.structures.insert(
                jackpot_structure_id,
                make_structure(jackpot_structure_id, char_corp_id, 35835, 31000007, "Wormhole Athanor", [1000.0, 2000.0, 3000.0]),
            );
            let mut metenox = make_structure(metenox_id, char_corp_id, 81826, 30000012, "Asabona Metenox", [200_000.0, 0.0, -100_000.0]);
            metenox.fuel_blocks = 200;
            snap.structures.insert(metenox_id, metenox);
            snap.items.insert(
                800_000_701,
                make_item(800_000_701, 81143, char_corp_id, metenox_id, InventoryFlag::Hangar, 500, false), // 500 Magmatic Gas
            );
            let mut outsider_ch = snap.characters[&char_id].clone();
            outsider_ch.character_id = 90_000_002;
            outsider_ch.corporation_id = 98_999_999;
            outsider_ch.structure_id = Some(moon_structure_id);
            snap.characters.insert(90_000_002, outsider_ch);
            let mut unpriv_ch = snap.characters[&char_id].clone();
            unpriv_ch.character_id = 90_000_003;
            unpriv_ch.structure_id = Some(moon_structure_id);
            snap.characters.insert(90_000_003, unpriv_ch);
            snap.characters.get_mut(&char_id).unwrap().structure_id = Some(moon_structure_id);
            snap.corporations.get_mut(&char_corp_id).unwrap().member_roles.insert(90_000_003, 0);
        });

        // Corp ACL denial: outsider (foreign corp) and unprivileged member (role 0) must be denied StartNewExtraction & FractureChunkWithStructure
        for (denied_cid, denied_corp) in [(90_000_002i64, 98_999_999i64), (90_000_003i64, char_corp_id)] {
            let mut d_sess = sess.clone();
            d_sess.character_id = Some(denied_cid);
            d_sess.corporation_id = Some(denied_corp);
            d_sess.structure_id = Some(moon_structure_id);
            d_sess.corp_role = 0;
            assert_eq!(ctx.dispatch_rpc(&mut d_sess, "moonExtractions", None, "StartNewExtraction", &[PyRep::Integer(518_400)], &kw, None).await.unwrap_err().code, "AccessDenied");
            assert_eq!(ctx.dispatch_rpc(&mut d_sess, "moonExtractions", None, "FractureChunkWithStructure", &[], &kw, None).await.unwrap_err().code, "AccessDenied");
        }

        sess.structure_id = Some(moon_structure_id);
        ctx.drain_notifications_for_char(char_id);
        let start_cancel = ctx
            .dispatch_rpc(&mut sess, "moonExtractions", None, "StartNewExtraction", &[PyRep::Integer(518_400)], &kw, None)
            .await
            .expect("StartNewExtraction before cancel");
        assert_eq!(get_dict_val(&start_cancel, "status").unwrap().as_str(), Some("Started"));
        assert!(
            ctx.drain_notifications_for_char(char_id).iter().any(|n| n.event == "OnMoonExtractionUpdate"),
            "StartNewExtraction must emit OnMoonExtractionUpdate"
        );

        let cancel_res = ctx
            .dispatch_rpc(&mut sess, "moonExtractions", None, "CancelExtraction", &[], &kw, None)
            .await
            .expect("0-arg CancelExtraction from session structure_id");
        assert_eq!(cancel_res, PyRep::Bool(true));
        assert!(
            ctx.drain_notifications_for_char(char_id)
                .iter()
                .any(|n| n.event == "OnMoonExtractionUpdate" && n.args == vec![PyRep::None]),
            "CancelExtraction must emit OnMoonExtractionUpdate(None)"
        );

        let dur_secs = 864_000u64; // 10 days (240h -> 7,200,000 m³ chunk)
        let start_res = ctx
            .dispatch_rpc(&mut sess, "moonExtractions", None, "StartNewExtraction", &[PyRep::Integer(dur_secs as i64)], &kw, None)
            .await
            .expect("StartNewExtraction 10d");
        let PyRep::List(planned_ores) = get_dict_val(&start_res, "moonOres").unwrap() else { panic!() };
        assert_eq!(planned_ores.len(), 3, "Moon composition must contain 3 SDE moon ores");

        // Firing moon drill laser before chunkArrivalTime must fail with ChunkNotReadyToFracture
        let early_err = ctx
            .dispatch_rpc(&mut sess, "moonExtractions", None, "FractureChunkWithStructure", &[], &kw, None)
            .await
            .expect_err("0-arg FractureChunkWithStructure before chunkArrivalTime must fail");
        assert_eq!(early_err.code, "ChunkNotReadyToFracture");

        ctx.advance_secs(dur_secs + 1);
        let non_jp_seed = (1u64..10_000)
            .find(|&s| eve_server::rng::MechanicRng::seeded(s).try_unit_f64().unwrap() >= 0.15)
            .unwrap();
        ctx.set_mechanic_rng_seed(Some(non_jp_seed));
        ctx.drain_notifications_for_char(char_id);
        let frac_res = ctx
            .dispatch_rpc(&mut sess, "moonExtractions", None, "FractureChunkWithStructure", &[], &kw, None)
            .await
            .expect("0-arg FractureChunkWithStructure");
        assert_eq!(
            ctx.dispatch_rpc(&mut sess, "moonExtractions", None, "CancelExtraction", &[], &kw, None).await.unwrap_err().code,
            "CannotCancelExtraction",
            "CancelExtraction must be rejected after chunk has fractured"
        );
        let frac_notifs = ctx.drain_notifications_for_char(char_id);
        let frac_update = frac_notifs
            .iter()
            .find(|n| n.event == "OnMoonExtractionUpdate")
            .expect("FractureChunkWithStructure must emit OnMoonExtractionUpdate");
        assert_eq!(get_dict_val(&frac_update.args[0], "status").and_then(|v| v.as_str()), Some("Fractured"));
        assert!(get_dict_val(&frac_update.args[0], "moonID").and_then(|v| v.as_i64()).is_some(), "OnMoonExtractionUpdate must include moonID");
        assert!(get_dict_val(&frac_update.args[0], "extractionStartTime").and_then(|v| v.as_i64()).is_some(), "OnMoonExtractionUpdate must include extractionStartTime");

        let PyRep::List(ast_ids) = get_dict_val(&frac_res, "asteroidIDs").unwrap() else { panic!() };
        let PyRep::List(ore_types) = get_dict_val(&frac_res, "oreTypes").unwrap() else { panic!() };
        assert_eq!(ast_ids.len(), 6, "Non-jackpot fracture spawns 6 asteroids (base + +15% Brimful variant for each of the 3 moon ores)");
        assert!(ore_types.iter().any(|t| ctx.sde.types[&(t.as_i64().unwrap() as u32)].name.starts_with("Brimful ")));

        // Mine the fractured moon asteroid 2 cycles and 2 belt ores (Veldspar + Scordite) so 3 distinct ore types are logged
        let first_moon_ast = ast_ids[0].as_i64().unwrap();
        let first_moon_type = ore_types[0].as_i64().unwrap() as u32;
        let miner_mod_id = 800_000_702i64;
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ship) = snap.items.get_mut(&ship_id) {
                ship.type_id = 32880; // Venture
            }
            snap.items.retain(|_, i| i.location_id != ship_id);
            snap.items.insert(miner_mod_id, make_item(miner_mod_id, 483, char_id, ship_id, InventoryFlag::HiSlot0, 1, true));
        });
        ctx.invalidate_ship_fitting(ship_id);

        let moon_ast_pos = jita.get_ball_snapshot(first_moon_ast).await.unwrap().unwrap().1.position;
        if let Ok(Some((_, mut sb))) = jita.get_ball_snapshot(ship_id).await {
            sb.position = moon_ast_pos + DVec3::new(500.0, 0.0, 0.0);
            let _ = jita.spawn_ball(sb).await;
        }
        ctx.db.with_mut_snapshot(|snap| snap.characters.get_mut(&char_id).unwrap().structure_id = None);
        sess.structure_id = None;
        let moon_c1 = crate::mining_cycle_engine::mine_once(&ctx, &mut sess, miner_mod_id, first_moon_ast).await.expect("Moon cycle 1");
        let moon_c2 = crate::mining_cycle_engine::mine_once(&ctx, &mut sess, miner_mod_id, first_moon_ast).await.expect("Moon cycle 2");
        let total_moon_mined = moon_c1.units_delivered + moon_c2.units_delivered;
        assert!(total_moon_mined > 0);

        // Advance past V24 48h (172,800s) natural decay window, run production tick_background_cluster, and verify moon asteroid belt despawns
        ctx.advance_secs(172_800);
        eve_server::session_cluster_lifecycle::tick_background_cluster(&ctx).await;
        ctx.db.with_mut_snapshot(|snap| snap.characters.get_mut(&char_id).unwrap().structure_id = Some(moon_structure_id));
        sess.structure_id = Some(moon_structure_id);
        let PyRep::Tuple(decayed_tup) = ctx.dispatch_rpc(&mut sess, "moonExtractions", None, "GetExtractionAndEventIDFromStructureID", &[], &kw, None).await.expect("0-arg GetExtractionAndEventIDFromStructureID after 48h decay") else { panic!("Expected 2-tuple (extraction, eventID)") };
        assert_eq!(decayed_tup.len(), 2);
        assert_eq!(get_dict_val(&decayed_tup[0], "status").and_then(|v| v.as_str()), Some("Decayed"));
        assert_eq!(get_dict_val(&decayed_tup[0], "activeAsteroidCount").and_then(|v| v.as_i64()), Some(0));
        assert!(decayed_tup[1].as_i64().is_some());
        assert!(ctx.db.get_asteroid(first_moon_ast).is_none() && jita.get_ball_snapshot(first_moon_ast).await.unwrap().is_none(), "Expired moon asteroid field must despawn after 48h V24 duration");

        // Also verify Jackpot (+100% variants) and auto-fracture after chunkArrivalTime + 3h (10,800s) on jackpot_structure_id via tick_background_cluster
        ctx.db.with_mut_snapshot(|snap| snap.characters.get_mut(&char_id).unwrap().structure_id = Some(jackpot_structure_id));
        sess.structure_id = Some(jackpot_structure_id);
        ctx.dispatch_rpc(&mut sess, "moonExtractions", None, "StartNewExtraction", &[PyRep::Integer(518_400)], &kw, None)
            .await
            .expect("StartNewExtraction on jackpot_structure_id for auto-fracture");
        ctx.advance_secs(518_400 + 10_801);
        ctx.drain_notifications_for_char(char_id);
        eve_server::session_cluster_lifecycle::tick_background_cluster(&ctx).await;
        let PyRep::Tuple(auto_tup) = ctx
            .dispatch_rpc(&mut sess, "moonExtractions", None, "GetExtractionAndEventIDFromStructureID", &[], &kw, None)
            .await
            .expect("0-arg GetExtractionAndEventIDFromStructureID after 3h auto-fracture delay")
        else {
            panic!("Expected 2-tuple");
        };
        assert_eq!(get_dict_val(&auto_tup[0], "status").and_then(|v| v.as_str()), Some("Fractured"));
        assert_eq!(get_dict_val(&auto_tup[0], "autoFractured").and_then(|v| v.as_bool()), Some(true));
        let PyRep::List(auto_ast_ids) = get_dict_val(&auto_tup[0], "asteroidIDs").unwrap() else { panic!() };
        let first_auto_ast = auto_ast_ids[0].as_i64().unwrap();
        assert!(
            ctx.drain_notifications_for_char(char_id)
                .iter()
                .any(|n| n.event == "OnMoonExtractionUpdate" && get_dict_val(&n.args[0], "autoFractured").and_then(|v| v.as_bool()) == Some(true)),
            "Auto-fracture after 3h delay must emit OnMoonExtractionUpdate with autoFractured == true"
        );

        ctx.dispatch_rpc(&mut sess, "moonExtractions", None, "StartNewExtraction", &[PyRep::Integer(518_400)], &kw, None)
            .await
            .expect("StartNewExtraction on jackpot_structure_id");
        ctx.advance_secs(518_401);
        eve_server::session_cluster_lifecycle::tick_background_cluster(&ctx).await;
        assert!(ctx.db.get_asteroid(first_auto_ast).is_none(), "Previous auto-fractured field must still despawn after its 48h window even after a new extraction starts");
        let jp_seed = (1u64..10_000)
            .find(|&s| eve_server::rng::MechanicRng::seeded(s).try_unit_f64().unwrap() < 0.15)
            .unwrap();
        ctx.set_mechanic_rng_seed(Some(jp_seed));
        let jp_frac = ctx
            .dispatch_rpc(&mut sess, "moonExtractions", None, "FractureChunkWithStructure", &[], &kw, None)
            .await
            .expect("0-arg FractureChunkWithStructure jackpot");
        assert_eq!(get_dict_val(&jp_frac, "isJackpot").and_then(|v| v.as_bool()), Some(true));
        assert_eq!(get_dict_val(&jp_frac, "jackpotBonus").and_then(|v| v.as_f64()), Some(2.0));
        ctx.db.with_mut_snapshot(|snap| snap.characters.get_mut(&char_id).unwrap().structure_id = None);
        sess.structure_id = None;

        // 2. plan:MN-14: Metenox Moon Drill — server-driven hourly harvesting into SpecializedMineralHold (40% yield, 5 fuel + 10 Magmatic Gas/h)
        // Reset Metenox fuel/gas and clear SpecializedMineralHold so we measure exact 0h -> 24h -> fuel-out transitions on the production background cluster tick
        ctx.db.with_mut_snapshot(|snap| {
            snap.structures.get_mut(&metenox_id).unwrap().fuel_blocks = 200;
            snap.items.retain(|_, i| !(i.location_id == metenox_id && i.flag == InventoryFlag::SpecializedMineralHold));
            snap.items.insert(800_000_701, make_item(800_000_701, 81143, char_corp_id, metenox_id, InventoryFlag::Hangar, 500, false));
            snap.auto_moon_miners.remove(&metenox_id);
            snap.service_state_blobs.insert("metenox_last_tick_unix".into(), ctx.now_unix().to_string());
        });

        // Independently compute expected hourly 40% moon material yield from SDE type_materials for the Metenox moon's 3 ores (727 units/h)
        let PyRep::List(met_ores) = ctx.dispatch_rpc(&mut sess, "moonExtractions", None, "GetMoonResourcesForStructure", &[PyRep::Integer(metenox_id)], &kw, None).await.expect("GetMoonResourcesForStructure Metenox") else { panic!() };
        let mut sde_mat_totals: std::collections::BTreeMap<u32, f64> = std::collections::BTreeMap::new();
        for kv in &met_ores {
            let tid = get_dict_val(kv, "typeID").unwrap().as_i64().unwrap() as u32;
            let frac = get_dict_val(kv, "yield").unwrap().as_f64().unwrap();
            let portions = (30_000.0 * frac) / (ctx.sde.types[&tid].volume.max(0.1) * (ctx.sde.types[&tid].portion_size.max(1) as f64));
            for m in ctx.sde.type_materials[&tid].iter().filter(|m| ctx.sde.types[&m.material_type_id].group_id == 427) {
                *sde_mat_totals.entry(m.material_type_id).or_insert(0.0) += portions * (m.quantity as f64) * 0.40;
            }
        }
        let expected_hourly_sde: i64 = sde_mat_totals.values().map(|q| q.round() as i64).sum();
        assert_eq!(expected_hourly_sde, 727);

        let details_0h = ctx
            .dispatch_rpc(&mut sess, "autoMoonMining", None, "RequestMiningDetails", &[PyRep::Integer(metenox_id)], &kw, None)
            .await
            .expect("RequestMiningDetails at 0h");
        assert_eq!(get_dict_val(&details_0h, "efficiency").and_then(|v| v.as_f64()), Some(0.40));
        assert_eq!(get_dict_val(&details_0h, "fuelRemaining").and_then(|v| v.as_i64()), Some(200));
        assert_eq!(get_dict_val(&details_0h, "magmaticGasRemaining").and_then(|v| v.as_i64()), Some(500));
        assert!(get_dict_val(&details_0h, "availableFuel").is_some() && get_dict_val(&details_0h, "nextHarvestTime").is_some());
        assert!(get_dict_val(&details_0h, "outputMaterials").is_some() && get_dict_val(&details_0h, "remainingOutputBayCapacity").is_some());
        let hourly_yield = get_dict_val(&details_0h, "hourlyYield").and_then(|v| v.as_i64()).unwrap();
        assert_eq!(hourly_yield, expected_hourly_sde, "hourlyYield must match independently computed SDE 40% moon material yield");

        // RequestMiningCycleOutput is a read-only query returning a list of (type_id, qty, vol) tuples and does NOT trigger harvesting
        let cyc_rate_res = ctx
            .dispatch_rpc(&mut sess, "autoMoonMining", None, "RequestMiningCycleOutput", &[PyRep::Integer(metenox_id)], &kw, None)
            .await
            .expect("RequestMiningCycleOutput read-only query");
        let PyRep::List(rate_tuples) = &cyc_rate_res else { panic!("Expected List of (type_id, qty, vol) tuples") };
        let rate_sum: i64 = rate_tuples
            .iter()
            .map(|t| match t {
                PyRep::Tuple(el) => el[1].as_i64().unwrap(),
                _ => panic!("Expected Tuple"),
            })
            .sum();
        assert_eq!(rate_sum, expected_hourly_sde, "Read-only RequestMiningCycleOutput per-cycle quantities sum to SDE hourlyYield");
        let bay_qty_0h: i32 = ctx.db.with_snapshot(|snap| {
            snap.items
                .values()
                .filter(|i| i.location_id == metenox_id && i.flag == InventoryFlag::SpecializedMineralHold)
                .map(|i| i.quantity)
                .sum()
        });
        assert_eq!(bay_qty_0h, 0, "Zero output exists before advancing the server clock, even after calling RequestMiningCycleOutput");

        // Advance 24 simulated hours (86,400 seconds) and run production tick_background_cluster: harvests 24 hourly cycles into SpecializedMineralHold
        ctx.clock().advance_secs(24 * 3_600);
        eve_server::session_cluster_lifecycle::tick_background_cluster(&ctx).await;
        let details_24h = ctx
            .dispatch_rpc(&mut sess, "autoMoonMining", None, "RequestMiningDetails", &[PyRep::Integer(metenox_id)], &kw, None)
            .await
            .expect("RequestMiningDetails after 24h");
        assert_eq!(get_dict_val(&details_24h, "fuelRemaining").and_then(|v| v.as_i64()), Some(80), "24h consumes 120 fuel blocks (5/h -> 80 left)");
        assert_eq!(get_dict_val(&details_24h, "magmaticGasRemaining").and_then(|v| v.as_i64()), Some(260), "24h consumes 240 Magmatic Gas (10/h -> 260 left)");
        let bay_qty_24h: i64 = ctx.db.with_snapshot(|snap| {
            snap.items
                .values()
                .filter(|i| i.location_id == metenox_id && i.flag == InventoryFlag::SpecializedMineralHold)
                .map(|i| {
                    assert_eq!(ctx.sde.types[&i.type_id].group_id, 427, "Deposited item must be group 427 Moon Material");
                    i.quantity as i64
                })
                .sum()
        });
        assert_eq!(bay_qty_24h, expected_hourly_sde * 24, "24 simulated hours deposit 24x SDE hourly 40% yield into SpecializedMineralHold");
        assert!(
            ctx.db.with_snapshot(|snap| snap.observer_mining_ledger.get(&metenox_id).is_none_or(|v| v.is_empty())),
            "Automated Metenox harvesting must not write pilot character rows to observer_mining_ledger"
        );

        // Advance another 20 hours and run production tick_background_cluster: only 16 cycles run (80 / 5 = 16h) before fuel blocks hit 0 and harvesting halts with status OutOfFuel
        ctx.clock().advance_secs(20 * 3_600);
        eve_server::session_cluster_lifecycle::tick_background_cluster(&ctx).await;
        let details_exhausted = ctx
            .dispatch_rpc(&mut sess, "autoMoonMining", None, "RequestMiningDetails", &[PyRep::Integer(metenox_id)], &kw, None)
            .await
            .expect("RequestMiningDetails after fuel exhaustion");
        assert_eq!(get_dict_val(&details_exhausted, "fuelRemaining").and_then(|v| v.as_i64()), Some(0));
        assert_eq!(get_dict_val(&details_exhausted, "status").and_then(|v| v.as_str()), Some("OutOfFuel"));
        let bay_qty_44h: i64 = ctx.db.with_snapshot(|snap| {
            snap.items
                .values()
                .filter(|i| i.location_id == metenox_id && i.flag == InventoryFlag::SpecializedMineralHold)
                .map(|i| i.quantity as i64)
                .sum()
        });
        assert_eq!(bay_qty_44h, expected_hourly_sde * 40, "Harvesting halts after 40 total funded hours when fuel is exhausted");

        // 3. plan:MN-15: Mining Sites — Ore anomaly, ice belt, and gas site spawn with SDE ore/ice/gas sets, probe scan resolution & despawn after depletion
        let osmon_id = 30_000_180u32; // Osmon (0.7 high-sec)
        mining_sites::seed_system_mining_sites_from_sde(&ctx, osmon_id).await;
        mining_sites::seed_system_mining_sites_from_sde(&ctx, 30_002_537).await; // Amamake (Low-Sec -> Saintly Nebula gas site)
        let ice_belt = mining_sites::resolve_regional_ice_belt(&ctx.sde, osmon_id).unwrap().expect("Blue Ice Belt in Osmon");
        assert_eq!((ice_belt.name.as_str(), ice_belt.asteroids.len(), ice_belt.ore_types(), mining_sites::ICE_BELT_RESPAWN_DELAY_SECS), ("Blue Ice Belt", 21, vec![16264], 14_400));
        let (gas_sig_id, gas_pos) = ctx.db.with_snapshot(|snap| snap.cosmic_signatures[&30_002_537].iter().find(|s| s.sig_group == "Gas" && s.type_name == "Saintly Nebula").map(|s| (s.signature_id.clone(), DVec3::from_array(s.position))).expect("Saintly Nebula gas sig"));
        ctx.db.with_mut_snapshot(|snap| snap.characters.get_mut(&char_id).unwrap().solar_system_id = 30_002_537);
        sess.solar_system_id = Some(30_002_537);
        let ls_scan_rows = crate::common::probe_scan(&ctx, &mut sess, gas_pos, 0.25 * 149_597_870_700.0).await;
        assert_eq!(crate::common::scan_row_field(&ls_scan_rows, &gas_sig_id, "signalStrength").as_f64(), Some(1.0), "scanMgr.RequestScans must resolve Saintly Nebula gas signature to 100%");
        assert_eq!(crate::common::scan_row_field(&ls_scan_rows, &gas_sig_id, "type").as_str(), Some("Saintly Nebula"));
        ctx.db.with_mut_snapshot(|snap| snap.characters.get_mut(&char_id).unwrap().solar_system_id = osmon_id);
        sess.solar_system_id = Some(osmon_id);
        let osmon = ctx.systems.get_or_spawn(osmon_id);

        let scan_before = ctx.dispatch_rpc(&mut sess, "scanSvc", None, "GetSystemScanMgr", &[], &kw, None).await.expect("GetSystemScanMgr");
        let PyRep::List(anoms_before) = get_dict_val(&scan_before, "anomalies").unwrap() else { panic!() };
        let ore_anom = anoms_before
            .iter()
            .find(|a| get_dict_val(a, "siteName").and_then(|v| v.as_str()) == Some("Small Kernite and Omber Deposit"))
            .expect("Small Kernite and Omber Deposit anomaly in Osmon");
        assert!(ctx.db.with_snapshot(|snap| snap.cosmic_signatures[&osmon_id].iter().any(|s| s.sig_group == "Ore" && s.type_name == "Hidden Omber Deposit")), "Hidden Omber Deposit ore signature in Osmon");
        let PyRep::List(anom_ast_reps) = get_dict_val(ore_anom, "asteroidBallIDs").unwrap() else { panic!() };
        let anom_ast_ids: Vec<i64> = anom_ast_reps.iter().map(|v| v.as_i64().unwrap()).collect();
        assert_eq!(anom_ast_ids.len(), 18, "Osmon ore anomaly spawns 18 SDE dungeon asteroids");
        let mut anom_ore_types: Vec<u32> = ctx.db.with_snapshot(|snap| anom_ast_ids.iter().map(|id| snap.asteroids[id].type_id).collect());
        anom_ore_types.sort_unstable();
        anom_ore_types.dedup();
        assert_eq!(anom_ore_types, vec![20, 1227, 17452, 17453, 17867, 17868], "Small Kernite and Omber Deposit must spawn exact SDE Kernite & Omber variant set");

        // Clear all but the last anomaly rock, then mine the final rock to 0 and verify the anomaly despawns
        let last_anom_rock = *anom_ast_ids.last().unwrap();
        let last_rock_pos = ctx.db.with_snapshot(|snap| {
            let p = snap.asteroids[&last_anom_rock].position;
            DVec3::new(p.0, p.1, p.2)
        });
        ctx.db.with_mut_snapshot(|snap| {
            for &id in &anom_ast_ids[..anom_ast_ids.len() - 1] {
                if let Some(a) = snap.asteroids.get_mut(&id) {
                    a.quantity = 0;
                }
            }
            if let Some(a) = snap.asteroids.get_mut(&last_anom_rock) {
                a.quantity = 10;
            }
            snap.items.retain(|_, i| !(i.location_id == ship_id && i.flag == InventoryFlag::SpecializedOreHold));
        });
        osmon
            .spawn_ball(Ball {
                id: ship_id,
                type_id: 32880,
                position: last_rock_pos + DVec3::new(500.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 2.0,
                mass: 1_200_000.0,
                radius: 30.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            })
            .await
            .unwrap();
        let clear_cycle = crate::mining_cycle_engine::mine_once(&ctx, &mut sess, miner_mod_id, last_anom_rock)
            .await
            .expect("Mine final anomaly asteroid to 0");
        assert_eq!(clear_cycle.asteroid_remaining, 0);
        let scan_after = ctx.dispatch_rpc(&mut sess, "scanSvc", None, "GetSystemScanMgr", &[], &kw, None).await.expect("GetSystemScanMgr after clear");
        let PyRep::List(anoms_after) = get_dict_val(&scan_after, "anomalies").unwrap() else { panic!() };
        assert!(
            !anoms_after.iter().any(|a| get_dict_val(a, "siteName").and_then(|v| v.as_str()) == Some("Small Kernite and Omber Deposit")),
            "Cleared ore anomaly must despawn from scanSvc.GetSystemScanMgr"
        );

        // Verify regional Ice Belt ("Blue Ice Belt") spawned in Osmon cosmic_anomalies, depletes & despawns, and respawns after 4h (14,400s)
        let ice_anom = anoms_after
            .iter()
            .find(|a| get_dict_val(a, "siteName").and_then(|v| v.as_str()) == Some("Blue Ice Belt"))
            .expect("Blue Ice Belt cosmic anomaly spawned in Osmon");
        let PyRep::List(ice_ast_reps) = get_dict_val(ice_anom, "asteroidBallIDs").unwrap() else { panic!() };
        let ice_ast_ids: Vec<i64> = ice_ast_reps.iter().map(|v| v.as_i64().unwrap()).collect();
        assert_eq!(ice_ast_ids.len(), 21, "Blue Ice Belt spawns 21 SDE Blue Ice (16264) asteroids");
        assert!(ctx.db.with_snapshot(|snap| ice_ast_ids.iter().all(|id| snap.asteroids[id].type_id == 16264 && snap.asteroids[id].quantity == 1_000)));
        let last_ice_rock = *ice_ast_ids.last().unwrap();
        let last_ice_pos = ctx.db.with_snapshot(|snap| {
            let p = snap.asteroids[&last_ice_rock].position;
            DVec3::new(p.0, p.1, p.2)
        });
        let ice_harv_id = 800_000_720i64;
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.get_mut(&ship_id).unwrap().type_id = 22544; // Hulk (can fit Ice Harvester I)
            snap.items.insert(ice_harv_id, make_item(ice_harv_id, 16278, char_id, ship_id, InventoryFlag::HiSlot1, 1, true));
            snap.skills.get_mut(&char_id).unwrap().insert(16281, eve_db::TrainedSkillRecord { type_id: 16281, level: 5, skill_points: 256_000.0 });
            for &id in &ice_ast_ids[..ice_ast_ids.len() - 1] {
                if let Some(a) = snap.asteroids.get_mut(&id) {
                    a.quantity = 0;
                }
            }
            if let Some(a) = snap.asteroids.get_mut(&last_ice_rock) {
                a.quantity = 1;
            }
            snap.items.retain(|_, i| !(i.location_id == ship_id && i.flag == InventoryFlag::SpecializedOreHold));
        });
        ctx.invalidate_ship_fitting(ship_id);
        osmon.try_set_ball_position(ship_id, last_ice_pos + DVec3::new(500.0, 0.0, 0.0));
        let ice_clear = crate::mining_cycle_engine::mine_once(&ctx, &mut sess, ice_harv_id, last_ice_rock)
            .await
            .expect("Mine final Blue Ice asteroid to 0");
        assert_eq!(ice_clear.asteroid_remaining, 0);
        let scan_ice_cleared = ctx.dispatch_rpc(&mut sess, "scanSvc", None, "GetSystemScanMgr", &[], &kw, None).await.expect("GetSystemScanMgr after ice clear");
        let PyRep::List(anoms_ice_cleared) = get_dict_val(&scan_ice_cleared, "anomalies").unwrap() else { panic!() };
        assert!(
            !anoms_ice_cleared.iter().any(|a| get_dict_val(a, "siteName").and_then(|v| v.as_str()) == Some("Blue Ice Belt")),
            "Depleted Blue Ice Belt must despawn from scanSvc"
        );
        ctx.advance_secs(14_399);
        let scan_before_4h = ctx.dispatch_rpc(&mut sess, "scanSvc", None, "GetSystemScanMgr", &[], &kw, None).await.expect("GetSystemScanMgr at 4h - 1s");
        let PyRep::List(anoms_before_4h) = get_dict_val(&scan_before_4h, "anomalies").unwrap() else { panic!() };
        assert!(
            !anoms_before_4h.iter().any(|a| get_dict_val(a, "siteName").and_then(|v| v.as_str()) == Some("Blue Ice Belt")),
            "Blue Ice Belt must remain despawned at 14,399s (< 4h)"
        );
        ctx.advance_secs(1);
        let scan_at_4h = ctx.dispatch_rpc(&mut sess, "scanSvc", None, "GetSystemScanMgr", &[], &kw, None).await.expect("GetSystemScanMgr at 4h");
        let PyRep::List(anoms_at_4h) = get_dict_val(&scan_at_4h, "anomalies").unwrap() else { panic!() };
        let respawned_ice = anoms_at_4h
            .iter()
            .find(|a| get_dict_val(a, "siteName").and_then(|v| v.as_str()) == Some("Blue Ice Belt"))
            .expect("Blue Ice Belt must respawn 4 hours (14,400s) after depletion");
        let PyRep::List(respawned_ice_balls) = get_dict_val(respawned_ice, "asteroidBallIDs").unwrap() else { panic!() };
        assert_eq!(respawned_ice_balls.len(), 21, "Respawned Blue Ice Belt has 21 fresh Blue Ice asteroids");

        // 4. plan:MN-16: Character & Corp Mining Ledgers (90-day retention filter: 60d retained, 91d pruned + waste tracking + outsider CrpAccessDenied)
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.get_mut(&ship_id).unwrap().type_id = 32880;
            snap.items.remove(&ice_harv_id);
            snap.items.retain(|_, i| !(i.location_id == ship_id && i.flag == InventoryFlag::SpecializedOreHold));
        });
        ctx.invalidate_ship_fitting(ship_id);
        let scordite_rock = 750_000_710i64;
        ctx.db.spawn_asteroid(AsteroidRecord {
            asteroid_id: scordite_rock,
            solar_system_id: osmon_id,
            belt_id: 40_000_001,
            type_id: 1228, // Scordite
            quantity: 10_000,
            initial_quantity: 10_000,
            position: (last_ice_pos.x + 600.0, last_ice_pos.y, last_ice_pos.z),
            radius: 60.0,
        });
        let _ = crate::mining_cycle_engine::mine_once(&ctx, &mut sess, miner_mod_id, scordite_rock).await.expect("Mine Scordite");
        let retained_60d_ts = ctx.now_unix().saturating_sub(60 * 86_400);
        let expired_ts = ctx.now_unix().saturating_sub(91 * 86_400);
        ctx.db.record_mining_ledger_entry_with_waste(char_id, osmon_id, 17452, 777, 111, retained_60d_ts).expect("Record 60d Silvery Omber with waste");
        ctx.db.with_mut_snapshot(|snap| {
            snap.mining_ledger.push(eve_db::MiningLedgerRecord {
                character_id: char_id, solar_system_id: osmon_id, type_id: 18, quantity: 999, date: "2026-01-01".into(), timestamp_unix: expired_ts,
            });
            snap.observer_mining_ledger.entry(moon_structure_id).or_default().push(eve_db::ObserverMiningRecord {
                observer_id: moon_structure_id, character_id: char_id, recorded_corporation_id: char_corp_id, type_id: 18, quantity: 999, last_updated_date: "2026-01-01".into(), timestamp_unix: expired_ts,
            });
        });

        let char_ledger = ctx
            .dispatch_rpc(&mut sess, "characterMiningLedger", None, "GetCharacterLogs", &[], &kw, None)
            .await
            .expect("GetCharacterLogs");
        let PyRep::List(char_entries) = &char_ledger else { panic!() };
        assert!(
            !char_entries.iter().any(|e| get_dict_val(e, "typeID").and_then(|v| v.as_i64()) == Some(18)),
            "GetCharacterLogs must prune 91-day-old Plagioclase (18) entry"
        );
        let omber_60d_row = char_entries
            .iter()
            .find(|e| get_dict_val(e, "typeID").and_then(|v| v.as_i64()) == Some(17452))
            .expect("60-day-old Silvery Omber (17452) entry must be retained within 90-day ledger window");
        assert_eq!(get_dict_val(omber_60d_row, "quantity").and_then(|v| v.as_i64()), Some(777));
        assert_eq!(get_dict_val(omber_60d_row, "quantityWasted").and_then(|v| v.as_i64()), Some(111));
        assert_eq!(get_dict_val(omber_60d_row, "waste").and_then(|v| v.as_i64()), Some(111));
        let moon_char_row = char_entries
            .iter()
            .find(|e| get_dict_val(e, "typeID").and_then(|v| v.as_i64()) == Some(first_moon_type as i64))
            .expect("Aggregated moon ore row in character ledger");
        assert_eq!(get_dict_val(moon_char_row, "quantity").and_then(|v| v.as_i64()), Some(total_moon_mined));

        sess.corp_role = eve_db::corp_roles::ROLE_DIRECTOR;
        let corp_ledger = ctx
            .dispatch_rpc(&mut sess, "corpMiningLedger", None, "GetObserverLedger", &[PyRep::Integer(moon_structure_id)], &kw, None)
            .await
            .expect("GetObserverLedger for owner corp director");
        assert!(corp_ledger.is_crowset());
        let obs_rows = decode_crowset_rows(&corp_ledger);
        assert!(!obs_rows.iter().any(|r| r.get(2).and_then(|v| v.as_i64()) == Some(18)), "GetObserverLedger must exclude 91-day-old expired entries");
        let moon_obs_rows: Vec<_> = obs_rows
            .iter()
            .filter(|r| r.first().and_then(|v| v.as_i64()) == Some(char_id) && r.get(2).and_then(|v| v.as_i64()) == Some(first_moon_type as i64))
            .collect();
        assert_eq!(moon_obs_rows.len(), 1, "Both moon mining cycles aggregate into 1 observer ledger row");
        assert_eq!(moon_obs_rows[0].get(3).and_then(|v| v.as_i64()), Some(total_moon_mined));

        // Outsider character (different corporation) must be denied access to corp observer ledger
        let outsider_char = 90_000_002i64;
        ctx.db.with_mut_snapshot(|snap| {
            let mut ch = snap.characters[&char_id].clone();
            ch.character_id = outsider_char;
            ch.corporation_id = 98_999_999;
            snap.characters.insert(outsider_char, ch);
        });
        let mut outsider_sess = sess.clone();
        outsider_sess.character_id = Some(outsider_char);
        outsider_sess.corporation_id = Some(98_999_999);
        let outsider_err = ctx
            .dispatch_rpc(&mut outsider_sess, "corpMiningLedger", None, "GetObserverLedger", &[PyRep::Integer(moon_structure_id)], &kw, None)
            .await
            .expect_err("Outsider must be denied GetObserverLedger");
        assert_eq!(outsider_err.code, "CrpAccessDenied");
    }
}

pub(crate) mod sde_mining_static_data_distributions {
    // Phase 5M (`MN-01`, `MN-02`, `MN-09`, `MN-17`, `MN-18`) and `OF-28` closure integration tests.

    use std::collections::HashMap;

    use eve_db::{InventoryFlag, InventoryItemRecord};
    use eve_destiny::{Ball, Ballpark, MovementMode};
    use eve_machonet::SessionState;
    use eve_proto::PyRep;
    use eve_server::living_world::{LivingWorldConvoy, LivingWorldDirector, NpcArchetype};
    use eve_server::router::ServerContext;
    use eve_server::services::mining_sites::ICE_BELT_SITES;
    use eve_server::system_actor::{AsteroidState, populate_asteroid_belt_entities};
    use glam::DVec3;

    use crate::common::login_pilot;

    // ============================================================================
    // 1. MN-01: Mining static data in the SDE importer
    // ============================================================================
    #[tokio::test]
    async fn test_mn01_sde_mining_static_data_and_distributions() {
        let ctx = ServerContext::new();
        let sde = &ctx.sde;

        // Veldspar (1230) -> Tritanium (34) only
        let veldspar_mats = sde.type_materials_for(1230);
        assert_eq!(veldspar_mats.len(), 1);
        assert_eq!(veldspar_mats[0].material_type_id, 34);

        // Scordite (1228) -> Tritanium (34) + Pyerite (35)
        let scordite_mats: Vec<u32> = sde.type_materials_for(1228).iter().map(|m| m.material_type_id).collect();
        assert_eq!(scordite_mats, vec![34, 35]);

        // Ice type (Clear Icicle 16262) yields its isotope and fuel materials (16272, 16273, 16274, 16275)
        let ice_mats: Vec<u32> = sde.type_materials_for(16262).iter().map(|m| m.material_type_id).collect();
        for expected in [16272u32, 16273, 16274, 16275] {
            assert!(ice_mats.contains(&expected), "Clear Icicle must yield {expected}");
        }

        // Ore variant yields differ from base ore (Concentrated Veldspar 17470 vs Veldspar 1230)
        let conc_veld_mats = sde.type_materials_for(17470);
        assert_eq!(conc_veld_mats.len(), 1);
        assert!(conc_veld_mats[0].quantity > veldspar_mats[0].quantity);

        // Moon ores (R4-R64: groups 1884, 1920..=1923), Gas (711), and Prismaticite (randomized_type_materials)
        assert!(sde.types.values().any(|t| (1920..=1923).contains(&t.group_id) && t.published));
        assert!(sde.types.values().any(|t| t.group_id == 711 && t.published));
        assert!(!sde.randomized_type_materials.is_empty(), "Prismaticite randomized materials must be loaded");

        // Compressed counterparts & portionSize (V24 Batch Compressed Veldspar = 62516)
        assert_eq!(sde.compressible_types.get(&1230).copied(), Some(62516));
        assert_eq!(sde.get_type(1230).unwrap().portion_size, 100);

        // Regional ice belts & belt celestials with positions (Perimeter 30000144)
        assert!(!ICE_BELT_SITES.is_empty());
        let perim_belts: Vec<_> = sde.celestials_in_system(30_000_144).into_iter().filter(|c| c.group_id == 9).collect();
        assert!(!perim_belts.is_empty());
        assert!(perim_belts.iter().all(|b| b.position.length() > 0.0));

        // Mining modules (Miner I 483), crystals, drones (Mining Drone I 10246), MLU I (22542), Venture (32880)
        let miner_i = sde.get_type(483).unwrap();
        assert!(miner_i.default_attributes.contains_key(&77)); // miningAmount
        assert!(miner_i.default_attributes.contains_key(&73)); // duration
        assert!(miner_i.default_attributes.contains_key(&54)); // maxRange
        let venture = sde.get_type(32880).unwrap();
        assert!(venture.default_attributes.contains_key(&1556)); // specialOreHoldCapacity
    }

    // ============================================================================
    // 2. MN-02: Asteroid belts as balls & security/region ore distribution
    // ============================================================================
    #[tokio::test]
    async fn test_mn02_asteroid_belts_as_balls_highsec_and_nullsec() {
        // parity: feat:mining.belts-as-balls, plan:MN-02
        eve_testkit::attest(&["feat:mining.belts-as-balls"]);
        let ctx = ServerContext::new();
        let mut sess = SessionState::new(9102);
        login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
        let kw = HashMap::new();
        let _ = ctx
            .dispatch_rpc(&mut sess, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(90_000_001)], &kw, None)
            .await
            .unwrap();

        // High-sec system (Perimeter 30000144, highsec 0.90): belts contain ONLY high-sec ores (volume <= 0.35 m3)
        let mut highsec_bp = Ballpark::new(30_000_144);
        let highsec_states = populate_asteroid_belt_entities(&mut highsec_bp, 30_000_144, &ctx.sde);
        assert!(!highsec_states.is_empty());
        for st in &highsec_states {
            let ore = ctx.sde.get_type(st.type_id).unwrap();
            assert!(ore.volume <= 0.35, "High-sec belt must contain only high-sec ores, found {}", ore.name);
            let ball = highsec_bp.balls.get(&st.asteroid_id).expect("Asteroid ball in ballpark");
            assert!(ball.radius >= 60.0);
        }

        // 0.0 / Null-sec system (30000208 LZ-6SU): has null-sec ores (volume > 3.0 m3)
        let mut null_bp = Ballpark::new(30_000_208);
        let null_states = populate_asteroid_belt_entities(&mut null_bp, 30_000_208, &ctx.sde);
        assert!(!null_states.is_empty());
        assert!(
            null_states.iter().all(|st| ctx.sde.get_type(st.type_id).unwrap().volume > 3.0),
            "0.0 system must spawn null-sec ores"
        );

        // Undock in Perimeter and warp to an asteroid belt -> OnWarpStarted fires (OnWarpToAsteroidBelt is client_local)
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&90_000_001) {
                ch.solar_system_id = 30_000_144;
                ch.station_id = Some(60_003_766);
                if let Some(ship) = snap.items.get_mut(&ch.active_ship_id) {
                    ship.location_id = 60_003_766;
                }
            }
        });
        sess.solar_system_id = Some(30_000_144);
        sess.station_id = Some(60_003_766);
        let _ = ctx.dispatch_rpc(&mut sess, "stationSvc", None, "Undock", &[], &kw, None).await.unwrap();
        let perim_belt_id = ctx.sde.celestials_in_system(30_000_144).into_iter().find(|c| c.group_id == 9).unwrap().celestial_id;
        let _ = ctx.drain_notifications_for_char(90_000_001);
        ctx.dispatch_rpc(
            &mut sess,
            "beyonce",
            None,
            "CmdWarpToStuff",
            &[PyRep::String("item".into()), PyRep::Integer(perim_belt_id), PyRep::Real(0.0)],
            &kw,
            None,
        )
        .await
        .expect("CmdWarpToStuff to Perimeter belt");
        let notifs = ctx.drain_notifications_for_char(90_000_001);
        assert!(notifs.iter().any(|n| n.event == "OnWarpStarted"));
        assert!(!notifs.iter().any(|n| n.event == "OnWarpToAsteroidBelt"));
    }

    // ============================================================================
    // 3. OF-28: OnOreMined notification & characterMiningLedger RPC
    // ============================================================================
    #[tokio::test]
    async fn test_of28_mining_laser_fired_local_and_canonical_ledger_rpc() {
        // parity: notify:OnOreMined, rpc:characterMiningLedger.GetCharacterLogs
        eve_testkit::attest(&["notify:OnOreMined", "rpc:characterMiningLedger.GetCharacterLogs"]);
        let ctx = ServerContext::new();
        crate::common::pin_clock(&ctx);
        let char_id = 90_000_001i64;
        let mut sess = SessionState::new(9109);
        login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
        let kw = HashMap::new();
        let _ = ctx
            .dispatch_rpc(&mut sess, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(char_id)], &kw, None)
            .await
            .unwrap();

        // Non-catalog `charMiningLedger` service must not be dispatched
        let legacy_err = ctx
            .dispatch_rpc(&mut sess, "charMiningLedger", None, "GetMiningLedger", &[], &kw, None)
            .await;
        assert!(legacy_err.is_err(), "Non-catalog charMiningLedger must be rejected");

        let laser_id = ctx.db.with_mut_snapshot(|snap| {
            let active_ship = snap.characters.get(&char_id).unwrap().active_ship_id;
            snap.items.get_mut(&active_ship).unwrap().type_id = 32880; // Venture
            snap.items.retain(|_, i| i.location_id != active_ship);
            snap.skills.entry(char_id).or_default().insert(
                3386,
                eve_db::TrainedSkillRecord {
                    type_id: 3386, // Mining
                    level: 5,
                    skill_points: 256_000.0,
                },
            );
            snap.next_entity_id += 1;
            let id = snap.next_entity_id;
            snap.items.insert(
                id,
                InventoryItemRecord {
                    item_id: id,
                    type_id: 483, // Miner I
                    owner_id: char_id,
                    location_id: active_ship,
                    flag: InventoryFlag::HiSlot0,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
            id
        });

        let _ = ctx.dispatch_rpc(&mut sess, "stationSvc", None, "Undock", &[], &kw, None).await.unwrap();
        let ship_id = sess.ship_id.unwrap();
        ctx.invalidate_ship_fitting(ship_id);
        let jita = ctx.systems.get_or_spawn(30_000_142);
        let (_, ship_ball) = jita.get_ball_snapshot(ship_id).await.unwrap().unwrap();

        let ast_id = 750_999_001i64;
        let ast_pos = ship_ball.position + DVec3::new(2_000.0, 0.0, 0.0);
        ctx.db.spawn_asteroid(eve_db::AsteroidRecord {
            asteroid_id: ast_id,
            solar_system_id: 30_000_142,
            belt_id: 40_000_001,
            type_id: 1230,
            quantity: 10_000,
            initial_quantity: 10_000,
            position: (ast_pos.x, ast_pos.y, ast_pos.z),
            radius: 100.0,
        });
        jita.spawn_asteroid(
            AsteroidState {
                asteroid_id: ast_id,
                belt_id: 40_000_001,
                type_id: 1230,
                initial_units: 10_000,
                remaining_units: 10_000,
                unit_volume: 0.1,
                depleted: false,
                respawn_at_tick: None,
            },
            Ball {
                id: ast_id,
                type_id: 1230,
                position: ast_pos,
                velocity: DVec3::ZERO,
                max_velocity: 0.0,
                inertia: 0.0,
                mass: 1.0e8,
                radius: 100.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
        )
        .await
        .unwrap();

        let _ = ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "AddTarget", &[PyRep::Integer(ast_id)], &kw, None).await.unwrap();
        for _ in 0..10 {
            jita.step_manual(1.0).await.unwrap();
        }
        let _ = ctx.drain_notifications_for_char(char_id);
        let _ = ctx
            .dispatch_rpc(
                &mut sess,
                "dogmaIM",
                None,
                "Activate",
                &[PyRep::Integer(laser_id), PyRep::String("miningLaser".into()), PyRep::Integer(ast_id)],
                &kw,
                None,
            )
            .await
            .unwrap();
        ctx.advance_secs(65);
        let _ = eve_server::services::mining_cycle::process_mining_cycles(&ctx).await;

        let notifs = ctx.drain_notifications_for_char(char_id);
        assert!(notifs.iter().any(|n| n.event == "OnOreMined"), "OnOreMined must fire");

        let logs = ctx.dispatch_rpc(&mut sess, "characterMiningLedger", None, "GetCharacterLogs", &[], &kw, None).await.unwrap();
        let PyRep::List(rows) = logs else { panic!("Expected List from GetCharacterLogs") };
        assert!(!rows.is_empty());
        let kv_i64 = |rep: &PyRep, key: &str| -> Option<i64> {
            match rep {
                PyRep::KeyVal(m) => m.get(key).and_then(|v| v.as_i64()),
                PyRep::Dict(entries) => entries
                    .iter()
                    .find(|(k, _)| k.as_str() == Some(key))
                    .and_then(|(_, v)| v.as_i64()),
                _ => None,
            }
        };
        let entry = rows
            .iter()
            .find(|r| kv_i64(r, "typeID") == Some(1230))
            .expect("GetCharacterLogs must contain Veldspar (1230) entry");
        assert_eq!(kv_i64(entry, "solarSystemID"), Some(30_000_142));
        assert!(kv_i64(entry, "quantity").unwrap_or(0) > 0, "GetCharacterLogs quantity must be > 0");
    }

    // ============================================================================
    // 4. MN-09: Mining drones
    // ============================================================================
    #[tokio::test]
    async fn test_mn09_mining_drones_ore_hold_delivery_and_range_check() {
        // parity: plan:MN-09, feat:drones.mining-drones, rpc:entity.CmdMineRepeatedly
        eve_testkit::attest(&["plan:MN-09", "feat:drones.mining-drones", "rpc:entity.CmdMineRepeatedly"]);
        let ctx = ServerContext::new();
        crate::common::pin_clock(&ctx);
        let char_id = 90_000_001i64;
        let mut sess = SessionState::new(9109);
        login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
        let kw = HashMap::new();
        let _ = ctx
            .dispatch_rpc(&mut sess, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(char_id)], &kw, None)
            .await
            .unwrap();

        let (ship_id, drone_a, drone_b) = ctx.db.with_mut_snapshot(|snap| {
            let active_ship = snap.characters.get(&char_id).unwrap().active_ship_id;
            snap.items.get_mut(&active_ship).unwrap().type_id = 32880; // Venture (5,000 m3 SpecializedOreHold, 10 mbit drone bandwidth)
            snap.items.retain(|_, i| !(i.location_id == active_ship && (i.flag == InventoryFlag::SpecializedOreHold || i.flag == InventoryFlag::DroneBay)));
            snap.next_entity_id += 1;
            let prefill_id = snap.next_entity_id;
            snap.items.insert(
                prefill_id,
                InventoryItemRecord {
                    item_id: prefill_id,
                    type_id: 1230, // Veldspar (0.1 m3 -> 49,500 units = 4,950 m3, leaving 50 m3 = 500 units)
                    owner_id: char_id,
                    location_id: active_ship,
                    flag: InventoryFlag::SpecializedOreHold,
                    quantity: 49_500,
                    singleton: false,
                    version: 1,
                    ..Default::default()
                },
            );
            snap.next_entity_id += 1;
            let da = snap.next_entity_id;
            snap.items.insert(
                da,
                InventoryItemRecord {
                    item_id: da,
                    type_id: 10246, // Mining Drone I
                    owner_id: char_id,
                    location_id: active_ship,
                    flag: InventoryFlag::DroneBay,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
            snap.next_entity_id += 1;
            let db_id = snap.next_entity_id;
            snap.items.insert(
                db_id,
                InventoryItemRecord {
                    item_id: db_id,
                    type_id: 10246, // Mining Drone I
                    owner_id: char_id,
                    location_id: active_ship,
                    flag: InventoryFlag::DroneBay,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
            (active_ship, da, db_id)
        });

        let _ = ctx.dispatch_rpc(&mut sess, "stationSvc", None, "Undock", &[], &kw, None).await.unwrap();
        ctx.invalidate_ship_fitting(ship_id);
        let jita = ctx.systems.get_or_spawn(30_000_142);
        let (_, ship_ball) = jita.get_ball_snapshot(ship_id).await.unwrap().unwrap();

        // Launch 2 Mining Drone Is from the ship's drone bay
        let drone_pair = PyRep::List(vec![PyRep::Integer(drone_a), PyRep::Integer(drone_b)]);
        ctx.dispatch_rpc(&mut sess, "ship", None, "LaunchDrones", std::slice::from_ref(&drone_pair), &kw, None)
            .await
            .expect("ship.LaunchDrones 2 Mining Drone Is");

        // Out-of-range asteroid at 25 km (> 20 km default drone control range, < 35 km legacy limit) must be rejected
        let far_ast_id = 750_999_002i64;
        jita.spawn_asteroid(
            AsteroidState {
                asteroid_id: far_ast_id,
                belt_id: 40_000_001,
                type_id: 1230,
                initial_units: 5_000,
                remaining_units: 5_000,
                unit_volume: 0.1,
                depleted: false,
                respawn_at_tick: None,
            },
            Ball {
                id: far_ast_id,
                type_id: 1230,
                position: ship_ball.position + DVec3::new(25_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 0.0,
                inertia: 0.0,
                mass: 1.0e8,
                radius: 100.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
        )
        .await
        .unwrap();
        let far_err = ctx
            .dispatch_rpc(
                &mut sess,
                "entity",
                None,
                "CmdMineRepeatedly",
                &[drone_pair.clone(), PyRep::Integer(far_ast_id)],
                &kw,
                None,
            )
            .await;
        assert_eq!(
            far_err.expect_err("Out-of-range asteroid must be rejected for mining drones").code,
            "OutOfRange"
        );

        // Spawn in-range asteroid and run entity.CmdMineRepeatedly
        let near_ast_id = 750_999_003i64;
        jita.spawn_asteroid(
            AsteroidState {
                asteroid_id: near_ast_id,
                belt_id: 40_000_001,
                type_id: 1230,
                initial_units: 10_000,
                remaining_units: 10_000,
                unit_volume: 0.1,
                depleted: false,
                respawn_at_tick: None,
            },
            Ball {
                id: near_ast_id,
                type_id: 1230,
                position: ship_ball.position + DVec3::new(2_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 0.0,
                inertia: 0.0,
                mass: 1.0e8,
                radius: 100.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
        )
        .await
        .unwrap();

        ctx.dispatch_rpc(
            &mut sess,
            "entity",
            None,
            "CmdMineRepeatedly",
            &[drone_pair, PyRep::Integer(near_ast_id)],
            &kw,
            None,
        )
        .await
        .expect("entity.CmdMineRepeatedly 2 Mining Drone Is");

        let c1_dur = eve_server::services::combat_crimewatch::with_combat_runtime(&ctx, |rt| {
            assert_eq!(rt.active_drone_miners.len(), 2, "Both Mining Drone Is must be actively mining");
            rt.active_drone_miners.values().next().unwrap().duration_secs
        });

        // Cycle 1: 2 Mining Drone Is deposit Veldspar into SpecializedOreHold
        ctx.advance_secs(c1_dur);
        eve_server::session_cluster_lifecycle::step_repeating_drone_mining(&ctx, ctx.now_unix()).await;
        let hold_after_c1: i32 = ctx.db.with_snapshot(|snap| {
            snap.items
                .values()
                .filter(|i| i.location_id == ship_id && i.flag == InventoryFlag::SpecializedOreHold && i.type_id == 1230)
                .map(|i| i.quantity)
                .sum()
        });
        assert!(hold_after_c1 > 49_500, "Ore hold quantity ({hold_after_c1}) must increase after drone mining cycle");

        // Cycle 2: Ore hold is now full for another full drone cycle -> drones stop mining
        ctx.advance_secs(c1_dur);
        eve_server::session_cluster_lifecycle::step_repeating_drone_mining(&ctx, ctx.now_unix()).await;
        let active_after_full = eve_server::services::combat_crimewatch::with_combat_runtime(&ctx, |rt| {
            rt.active_drone_miners.len()
        });
        assert_eq!(active_after_full, 0, "Mining drones must stop mining when ore hold is full");
    }

    // ============================================================================
    // 5. DERIVED NPC mining op hauler replenishment
    // ============================================================================
    #[tokio::test]
    async fn test_npc_mining_op_derived_hauler_replenishment() {
        // parity: plan:MN-17
        let ctx = ServerContext::new();
        let director = LivingWorldDirector::new();

        let convoy_id = 800_000_199i64;
        let system_id = 30_000_144u32;
        let dest_station_id = 60_003_766i64;
        let ore_type_id = 1230u32;

        let handle = ctx.systems.get_or_spawn(system_id);
        let ast_id = 750_000_271i64;
        handle
            .spawn_asteroid(
                AsteroidState {
                    asteroid_id: ast_id,
                    belt_id: 40_000_001,
                    type_id: ore_type_id,
                    initial_units: 60_000,
                    remaining_units: 60_000,
                    unit_volume: 0.1,
                    depleted: false,
                    respawn_at_tick: None,
                },
                Ball {
                    id: ast_id,
                    type_id: ore_type_id,
                    position: DVec3::new(5_000.0, 0.0, 0.0),
                    velocity: DVec3::ZERO,
                    max_velocity: 0.0,
                    inertia: 0.0,
                    mass: 1.0e8,
                    radius: 50.0,
                    speed_fraction: 0.0,
                    mode: MovementMode::Stop,
                },
            )
            .await
            .unwrap();

        director.register_convoy(LivingWorldConvoy {
            convoy_id,
            archetype: NpcArchetype::MiningOp,
            ship_type_id: 32880,
            origin_system_id: system_id,
            current_system_id: system_id,
            destination_system_id: system_id,
            destination_station_id: dest_station_id,
            cargo_type_id: ore_type_id,
            cargo_quantity: 48_800,
            lod_level: 1,
            destroyed: false,
        });

        let vol_before: i32 = ctx.db.with_snapshot(|snap| {
            snap.market_orders
                .values()
                .filter(|o| !o.is_bid && o.is_npc && o.station_id == dest_station_id && o.type_id == ore_type_id)
                .map(|o| o.vol_remaining)
                .sum()
        });
        let _ = director.step_npc_mining_op(convoy_id, &ctx.systems, &ctx.db, &ctx.sde).await.unwrap();
        ctx.advance_secs(60);
        let step = director.step_npc_mining_op(convoy_id, &ctx.systems, &ctx.db, &ctx.sde).await.unwrap();
        assert!(step.mined_units > 0, "NPC mining op must deplete asteroid quantity over ticks");
        assert!(step.remaining_asteroid_units < 60_000);
        assert!(!step.hauled_to_station);
        ctx.advance_secs(60);
        let deliver_step = director.step_npc_mining_op(convoy_id, &ctx.systems, &ctx.db, &ctx.sde).await.unwrap();
        assert!(deliver_step.hauled_to_station);
        let vol_after: i32 = ctx.db.with_snapshot(|snap| {
            snap.market_orders
                .values()
                .filter(|o| !o.is_bid && o.is_npc && o.station_id == dest_station_id && o.type_id == ore_type_id)
                .map(|o| o.vol_remaining)
                .sum()
        });
        assert!(
            vol_after > vol_before,
            "NPC hauler delivery must increase NPC sell-order vol_remaining ({vol_before} -> {vol_after})"
        );
    }
}

pub(crate) mod gm_tournament_match_bans_cleanup {
    // Live integration tests for newly-closed feature rows (`EvoMu-w34`):
    // - `feat:admin.gm-tournament`
    // - `feat:serverops.session-changes`
    // - `feat:removed.medical-clone-grades`
    // Plus regression coverage for market-adjusted EIV and warable lookup filtering.

    use crate::common;
    use eve_db::{
        AllianceRecord, CorporationRecord, InventoryFlag, InventoryItemRecord, IskCents,
        MarketHistoryDayRecord, TrainedSkillRecord, UpwellStructureRecord,
    };
    use eve_machonet::{service_roles, SessionState};
    use eve_proto::{BridgeRpcError, PyRep};
    use eve_server::router::{cache_notifications::PendingMachoNotification, ServerContext};
    use std::collections::HashMap;

    const PILOT_A: i64 = 90_000_001;
    const PILOT_B: i64 = 90_000_002;
    const JITA_SYSTEM: u32 = 30_000_142;
    const JITA_STATION: i64 = 60_003_760;

    async fn login_pilot(ctx: &ServerContext, sid: u64, user: &str, pass: &str, char_id: i64) -> SessionState {
        let mut s = SessionState::new(sid);
        common::login_pilot(ctx, &mut s, user, pass).await;
        ctx.dispatch_rpc(
            &mut s,
            "charUnboundMgr",
            None,
            "SelectCharacterID",
            &[PyRep::Integer(char_id)],
            &HashMap::new(),
            None,
        )
        .await
        .expect("SelectCharacterID");
        s.station_id = Some(JITA_STATION);
        ctx.advance_secs(12);
        s
    }

    async fn rpc(ctx: &ServerContext, s: &mut SessionState, svc: &str, method: &str, args: &[PyRep]) -> Result<PyRep, BridgeRpcError> {
        ctx.dispatch_rpc(s, svc, None, method, args, &HashMap::new(), None).await
    }

    fn find_notifs<'a>(notifs: &'a [PendingMachoNotification], event: &str) -> Vec<&'a PendingMachoNotification> {
        notifs.iter().filter(|n| n.event == event).collect()
    }

    fn session_change_pair(notif: &PendingMachoNotification, key: &str) -> Option<(PyRep, PyRep)> {
        let PyRep::Tuple(pair) = notif.args.get(2)?.dict_get(key)? else { return None };
        (pair.len() == 2).then(|| (pair[0].clone(), pair[1].clone()))
    }

    fn make_item(
        item_id: i64,
        type_id: u32,
        owner_id: i64,
        location_id: i64,
        flag: InventoryFlag,
        quantity: i32,
        singleton: bool,
    ) -> InventoryItemRecord {
        InventoryItemRecord {
            item_id,
            type_id,
            owner_id,
            location_id,
            flag,
            quantity,
            singleton,
            version: 1,
            dynamic_attributes: HashMap::new(),
            source_mutaplasmid_id: None,
            insured_payout_isk: 0.0,
            is_blueprint_copy: false,
            blueprint_runs_remaining: None,
        }
    }

    #[tokio::test]
    async fn test_gm_tournament_match_bans_and_cleanup_grid() {
        // parity: feat:admin.gm-tournament
        eve_testkit::attest(&["feat:admin.gm-tournament"]);

        let ctx = ServerContext::new();
        let mut gm_sess = login_pilot(&ctx, 8901, "demo", "demo-secret", PILOT_A).await;
        let mut p2_sess = login_pilot(&ctx, 8902, "other", "other-secret", PILOT_B).await;

        // Negative 1: non-GM pilot cannot CreateMatch
        gm_sess.role = 0;
        let denied = rpc(
            &ctx,
            &mut gm_sess,
            "tourneyMgr",
            "CreateMatch",
            &[PyRep::Integer(1), PyRep::Integer(2401), PyRep::Integer(100), PyRep::Integer(600), PyRep::Integer(0), PyRep::Integer(0)],
        )
        .await
        .expect_err("non-GM CreateMatch must fail");
        assert!(!denied.code.is_empty());

        // 1. GM creates AT match with Red & Blue teams
        gm_sess.role = service_roles::ROLE_GMH;
        let create_rep = rpc(
            &ctx,
            &mut gm_sess,
            "tourneyMgr",
            "CreateMatch",
            &[
                PyRep::Integer(1),
                PyRep::Tuple(vec![
                    PyRep::Integer(2401),
                    PyRep::Integer(98_000_001),
                    PyRep::Integer(98_000_002),
                    PyRep::Integer(PILOT_A),
                    PyRep::Integer(PILOT_B),
                ]),
                PyRep::Integer(100),
                PyRep::Integer(600),
                PyRep::Integer(0),
                PyRep::Integer(0),
            ],
        )
        .await
        .expect("GM CreateMatch");
        let PyRep::Tuple(create_tup) = create_rep else { panic!("expected CreateMatch 3-tuple") };
        assert_eq!(create_tup[0], PyRep::Integer(2401));
        let PyRep::List(beacons) = &create_tup[1] else { panic!("expected beacon list") };
        assert_eq!(beacons.len(), 2);
        let beacon_1 = beacons[0].as_i64().expect("beacon 1 id");

        // Negative 2: non-referee pilot cannot AddPlayer or CleanupGrid
        let p2_add_err = rpc(&ctx, &mut p2_sess, "tourneyMgr", "AddPlayer", &[PyRep::Integer(2401), PyRep::Integer(1), PyRep::Integer(PILOT_B)])
            .await
            .expect_err("non-referee AddPlayer must fail");
        assert_eq!(p2_add_err.code, "AccessDenied");
        let p2_clean_err = rpc(&ctx, &mut p2_sess, "tourneyMgr", "CleanupGrid", &[PyRep::Integer(2401)])
            .await
            .expect_err("non-referee CleanupGrid must fail");
        assert_eq!(p2_clean_err.code, "AccessDenied");

        // 2. Referee adds Red and Blue pilots
        assert_eq!(
            rpc(&ctx, &mut gm_sess, "tourneyMgr", "AddPlayer", &[PyRep::Integer(2401), PyRep::Integer(0), PyRep::Integer(PILOT_A)]).await.expect("AddPlayer Red"),
            PyRep::Bool(true)
        );
        assert_eq!(
            rpc(&ctx, &mut gm_sess, "tourneyMgr", "AddPlayer", &[PyRep::Integer(2401), PyRep::Integer(1), PyRep::Integer(PILOT_B)]).await.expect("AddPlayer Blue"),
            PyRep::Bool(true)
        );

        // 3. BanShip + FinalizeBans enforce ship hull bans during pregame checks
        assert_eq!(
            rpc(&ctx, &mut gm_sess, "tourneyMgr", "BanShip", &[PyRep::Integer(2401), PyRep::List(vec![PyRep::Integer(587)])]).await.expect("BanShip"),
            PyRep::Bool(true)
        );
        assert_eq!(
            rpc(&ctx, &mut gm_sess, "tourneyMgr", "FinalizeBans", &[PyRep::Integer(2401)]).await.expect("FinalizeBans"),
            PyRep::Bool(true)
        );
        let post_fin_ban = rpc(&ctx, &mut gm_sess, "tourneyMgr", "BanShip", &[PyRep::Integer(2401), PyRep::List(vec![PyRep::Integer(597)])])
            .await
            .expect_err("BanShip after FinalizeBans must fail");
        assert_eq!(post_fin_ban.code, "TourneyBansFinalized");
        let post_fin_bantype = rpc(&ctx, &mut gm_sess, "tourneyMgr", "BanShipType", &[PyRep::Integer(2401), PyRep::Integer(0), PyRep::Integer(597)])
            .await
            .expect_err("BanShipType after FinalizeBans must fail");
        assert_eq!(post_fin_bantype.code, "TourneyBansFinalized");
        let banned_check = rpc(&ctx, &mut gm_sess, "tourneyMgr", "CompletePregameChecks", &[PyRep::Integer(2401)])
            .await
            .expect_err("pilot in banned Rifter (587) must fail CompletePregameChecks");
        assert!(!banned_check.code.is_empty());

        // Switch both pilots to legal Punisher (597) hulls, clear recorded ban errors via OverrideError, and complete pregame
        ctx.db.with_mut_snapshot(|snap| {
            for (idx, cid) in [PILOT_A, PILOT_B].into_iter().enumerate() {
                let sid = 930_000_100 + idx as i64;
                snap.items.insert(sid, make_item(sid, 597, cid, JITA_STATION, InventoryFlag::Hangar, 1, true));
                if let Some(ch) = snap.characters.get_mut(&cid) {
                    ch.active_ship_id = sid;
                }
            }
        });
        for cid in [PILOT_A, PILOT_B] {
            rpc(&ctx, &mut gm_sess, "tourneyMgr", "OverrideError", &[PyRep::Integer(2401), PyRep::Integer(cid), PyRep::String(String::new())])
                .await
                .expect("OverrideError");
        }
        assert_eq!(
            rpc(&ctx, &mut gm_sess, "tourneyMgr", "CompletePregameChecks", &[PyRep::Integer(2401)]).await.expect("CompletePregameChecks"),
            PyRep::Bool(true)
        );
        let sys_handle = ctx.systems.get_or_spawn(JITA_SYSTEM);
        assert!(sys_handle.get_ball_snapshot(beacon_1).await.unwrap().is_some());
        rpc(&ctx, &mut gm_sess, "tourneyMgr", "StartCountdown", &[PyRep::Integer(2401)]).await.expect("StartCountdown");
        rpc(&ctx, &mut gm_sess, "tourneyMgr", "MatchCompleted", &[PyRep::Integer(2401), PyRep::Integer(0)]).await.expect("MatchCompleted");
        assert!(sys_handle.get_ball_snapshot(beacon_1).await.unwrap().is_none());

        // 4. CleanupGrid removes arena-grid wrecks + cargo from both DB items and live ballpark while preserving ships and off-grid containers
        let (wreck_id, loot_id, ship_on_grid, offgrid_can_id) = (930_000_500_i64, 930_000_501_i64, 930_000_502_i64, 930_000_503_i64);
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(wreck_id, make_item(wreck_id, 26557, PILOT_A, i64::from(JITA_SYSTEM), InventoryFlag::None, 1, true));
            snap.items.insert(loot_id, make_item(loot_id, 34, PILOT_A, wreck_id, InventoryFlag::Cargo, 1_000, false));
            snap.items.insert(ship_on_grid, make_item(ship_on_grid, 587, PILOT_A, i64::from(JITA_SYSTEM), InventoryFlag::None, 1, true));
            snap.items.insert(offgrid_can_id, make_item(offgrid_can_id, 23, PILOT_A, i64::from(JITA_SYSTEM), InventoryFlag::None, 1, true));
            snap.item_positions.insert(offgrid_can_id, [5_000_000.0, 0.0, 0.0]);
        });
        for (bid, tid, gid, x_pos) in [(wreck_id, 26557_u32, 186_u32, 1_500.0), (ship_on_grid, 587_u32, 25_u32, 1_500.0), (offgrid_can_id, 23_u32, 12_u32, 5_000_000.0)] {
            let _ = sys_handle
                .spawn_ball_with_meta(
                    eve_destiny::Ball {
                        id: bid,
                        type_id: tid,
                        position: glam::DVec3::new(x_pos, 0.0, 0.0),
                        velocity: glam::DVec3::ZERO,
                        max_velocity: 0.0,
                        inertia: 1.0,
                        mass: 1_000.0,
                        radius: 25.0,
                        speed_fraction: 0.0,
                        mode: eve_destiny::MovementMode::Stop,
                    },
                    ctx.build_type_combat_state(tid),
                    eve_destiny::SlimItemMeta {
                        group_id: gid,
                        category_id: if gid == 25 { 6 } else { 2 },
                        owner_id: PILOT_A,
                        char_id: None,
                        corp_id: -1,
                        alliance_id: -1,
                        security_status: 0.0,
                        is_cloaked: false,
                        skin_id: None,
                        skin_material_set_id: None,
                        jump_target: None,
                    },
                )
                .await;
        }
        assert!(sys_handle.get_ball_snapshot(wreck_id).await.unwrap().is_some());

        assert_eq!(
            rpc(&ctx, &mut gm_sess, "tourneyMgr", "CleanupGrid", &[PyRep::Integer(2401)]).await.expect("CleanupGrid"),
            PyRep::Bool(true)
        );
        assert!(ctx.db.with_snapshot(|s| !s.items.contains_key(&wreck_id) && !s.items.contains_key(&loot_id) && s.items.contains_key(&ship_on_grid) && s.items.contains_key(&offgrid_can_id)));
        assert!(sys_handle.get_ball_snapshot(wreck_id).await.unwrap().is_none());
        assert!(sys_handle.get_ball_snapshot(ship_on_grid).await.unwrap().is_some());
        assert!(sys_handle.get_ball_snapshot(offgrid_can_id).await.unwrap().is_some());
    }

    #[tokio::test]
    async fn test_session_attribute_changes_propagate_across_location_ship_corp_and_roles() {
        // parity: feat:serverops.session-changes
        eve_testkit::attest(&["feat:serverops.session-changes"]);

        let ctx = ServerContext::new();
        let mut sess_a = login_pilot(&ctx, 8903, "demo", "demo-secret", PILOT_A).await;
        let mut sess_b = login_pilot(&ctx, 8904, "other", "other-secret", PILOT_B).await;

        // 1. Location propagation: Undock -> in-space session + OnSessionChanged -> CmdDock -> docked session
        let _ = ctx.drain_notifications_for_char(PILOT_A);
        rpc(&ctx, &mut sess_a, "ship", "Undock", &[]).await.expect("Undock");
        assert_eq!(sess_a.station_id, None);
        assert_eq!(sess_a.solar_system_id, Some(JITA_SYSTEM));
        let undock_notifs = ctx.drain_notifications_for_char(PILOT_A);
        let undock_sc = find_notifs(&undock_notifs, "OnSessionChanged");
        assert!(undock_sc.iter().any(|n| session_change_pair(n, "stationid") == Some((PyRep::Integer(JITA_STATION), PyRep::None))));
        assert!(rpc(&ctx, &mut sess_a, "jumpCloneSvc", "InstallCloneInStation", &[]).await.is_err());

        ctx.advance_secs(12);
        let _ = ctx.drain_notifications_for_char(PILOT_A);
        rpc(&ctx, &mut sess_a, "beyonce", "CmdDock", &[PyRep::Integer(JITA_STATION)]).await.expect("CmdDock");
        assert_eq!(sess_a.station_id, Some(JITA_STATION));
        let dock_notifs = ctx.drain_notifications_for_char(PILOT_A);
        let dock_sc = find_notifs(&dock_notifs, "OnSessionChanged");
        assert!(dock_sc.iter().any(|n| session_change_pair(n, "stationid") == Some((PyRep::None, PyRep::Integer(JITA_STATION)))));

        // 2. Ship propagation: ship.Board updates session.ship_id, emits OnSessionChanged("shipid"), and syncs character sheet
        ctx.advance_secs(12);
        let old_ship_a = sess_a.ship_id.expect("initial ship_id");
        let slasher_id = 931_000_001_i64;
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(slasher_id, make_item(slasher_id, 585, PILOT_A, JITA_STATION, InventoryFlag::Hangar, 1, true));
        });
        let _ = ctx.drain_notifications_for_char(PILOT_A);
        rpc(&ctx, &mut sess_a, "ship", "Board", &[PyRep::Integer(slasher_id)]).await.expect("ship.Board");
        assert_eq!(sess_a.ship_id, Some(slasher_id));
        let board_notifs = ctx.drain_notifications_for_char(PILOT_A);
        let board_sc = find_notifs(&board_notifs, "OnSessionChanged");
        assert!(board_sc.iter().any(|n| session_change_pair(n, "shipid") == Some((PyRep::Integer(old_ship_a), PyRep::Integer(slasher_id)))));
        let info_a = rpc(&ctx, &mut sess_a, "charMgr", "GetCharacterInfo", &[]).await.expect("GetCharacterInfo");
        assert_eq!(info_a.dict_get("shipID"), Some(&PyRep::Integer(slasher_id)));
        assert_eq!(info_a.dict_get("shipTypeID"), Some(&PyRep::Integer(585)));

        // 3. Corp propagation: PILOT_B joins CEO PILOT_A's player corporation -> OnSessionChanged("corpid") + auto-sync
        let player_corp_id = 98_500_001_i64;
        ctx.db.with_mut_snapshot(|snap| {
            let mut corp = CorporationRecord {
                corporation_id: player_corp_id,
                name: "Session Propagation Corp".into(),
                ticker: "SPROP".into(),
                ceo_id: PILOT_A,
                hq_station_id: Some(JITA_STATION),
                ..Default::default()
            };
            corp.members.insert(PILOT_A);
            corp.member_roles.insert(PILOT_A, u64::MAX);
            snap.corporations.insert(player_corp_id, corp);
            if let Some(ch) = snap.characters.get_mut(&PILOT_A) {
                ch.corporation_id = player_corp_id;
            }
            if let Some(ch) = snap.characters.get_mut(&PILOT_B) {
                ch.station_id = Some(JITA_STATION);
            }
        });
        sess_a.corporation_id = Some(player_corp_id);
        sess_a.corp_role = u64::MAX;
        ctx.advance_secs(12);

        let old_corp_b = sess_b.corporation_id.expect("initial NPC corp");
        let app_rep = rpc(
            &ctx,
            &mut sess_b,
            "corpRegistry",
            "InsertApplication",
            &[PyRep::Integer(player_corp_id), PyRep::String("Applying".into())],
        )
        .await
        .expect("InsertApplication");
        let app_id = app_rep.dict_get("applicationID").and_then(|v| v.as_i64()).expect("applicationID");
        let _ = ctx.drain_notifications_for_char(PILOT_B);

        rpc(
            &ctx,
            &mut sess_a,
            "corpRegistry",
            "UpdateApplicationOffer",
            &[PyRep::Integer(app_id), PyRep::String("Accepted".into())],
        )
        .await
        .expect("UpdateApplicationOffer Accepted");

        let join_notifs = ctx.drain_notifications_for_char(PILOT_B);
        let join_sc = find_notifs(&join_notifs, "OnSessionChanged");
        assert!(join_sc.iter().any(|n| session_change_pair(n, "corpid") == Some((PyRep::Integer(old_corp_b), PyRep::Integer(player_corp_id)))));

        // 4. Roles propagation: unprivileged member denied -> CEO UpdateMember grants role -> OnSessionChanged("corprole") + live sync -> revoke blocks again
        let corp_fit = PyRep::keyval([
            ("name", PyRep::String("Doctrine Punisher".into())),
            ("shipTypeID", PyRep::Integer(597)),
            ("description", PyRep::String("Role-gated corp fit".into())),
            ("fitData", PyRep::List(vec![])),
        ]);
        assert!(
            rpc(&ctx, &mut sess_b, "corpFittingMgr", "SaveFitting", &[PyRep::Integer(player_corp_id), corp_fit.clone()])
                .await
                .is_err()
        );
        assert_eq!(sess_b.corporation_id, Some(player_corp_id));

        let fitting_mgr_role: u64 = 1u64 << 41;
        let hq_role: u64 = 1u64 << 7;
        let delayed_hangar_role: u64 = 1u64 << 13; // Hangar Take 1 (24h delayed grant)
        let _ = ctx.drain_notifications_for_char(PILOT_B);
        rpc(
            &ctx,
            &mut sess_a,
            "corpRegistry",
            "UpdateMember",
            &[PyRep::keyval([
                ("characterID", PyRep::Integer(PILOT_B)),
                ("roles", PyRep::Integer((fitting_mgr_role | delayed_hangar_role) as i64)),
                ("rolesAtHQ", PyRep::Integer(hq_role as i64)),
            ])],
        )
        .await
        .expect("UpdateMember grant");

        let grant_notifs = ctx.drain_notifications_for_char(PILOT_B);
        let grant_sc = find_notifs(&grant_notifs, "OnSessionChanged");
        let role_notif = grant_sc
            .iter()
            .find(|n| session_change_pair(n, "corprole") == Some((PyRep::Integer(0), PyRep::Integer(fitting_mgr_role as i64))))
            .expect("OnSessionChanged with immediate corprole");
        assert_eq!(role_notif.id_type, "charid", "OnSessionChanged must address charid");
        assert_eq!(role_notif.args[1].dict_get("charid"), Some(&PyRep::Integer(PILOT_B)));
        assert_eq!(role_notif.args[1].dict_get("corpid"), Some(&PyRep::Integer(player_corp_id)));
        assert_eq!(role_notif.args[1].dict_get("corprole"), Some(&PyRep::Integer(fitting_mgr_role as i64)));
        assert_eq!(role_notif.args[1].dict_get("rolesAtHQ"), Some(&PyRep::Integer(hq_role as i64)));
        assert_eq!(session_change_pair(role_notif, "rolesAtHQ"), Some((PyRep::Integer(0), PyRep::Integer(hq_role as i64))));

        let saved_fid = rpc(&ctx, &mut sess_b, "corpFittingMgr", "SaveFitting", &[PyRep::Integer(player_corp_id), corp_fit.clone()])
            .await
            .expect("member live session must pick up granted role")
            .as_i64()
            .expect("saved corp fitting id");
        assert!(saved_fid > 0);
        assert_eq!(sess_b.corp_role, fitting_mgr_role);

        // Non-Director Fitting Manager can also call corpFittingMgr.UpdateNameAndDescription
        let renamed = rpc(
            &ctx,
            &mut sess_b,
            "corpFittingMgr",
            "UpdateNameAndDescription",
            &[
                PyRep::Integer(saved_fid),
                PyRep::Integer(player_corp_id),
                PyRep::String("Renamed Doctrine Punisher".into()),
                PyRep::String("Updated by Fitting Manager".into()),
            ],
        )
        .await
        .expect("Fitting Manager UpdateNameAndDescription");
        assert_eq!(renamed.dict_get("name"), Some(&PyRep::String("Renamed Doctrine Punisher".into())));

        // Advance 24h so pending delayed hangar role matures and emits OnSessionChanged
        let _ = ctx.drain_notifications_for_char(PILOT_B);
        ctx.advance_secs(86_405);
        let _ = rpc(&ctx, &mut sess_b, "corpRegistry", "GetMember", &[PyRep::Integer(PILOT_B)]).await.expect("GetMember triggers pending grant promotion");
        let matured_notifs = ctx.drain_notifications_for_char(PILOT_B);
        let matured_sc = find_notifs(&matured_notifs, "OnSessionChanged");
        assert!(
            matured_sc.iter().any(|n| n.id_type == "charid"
                && session_change_pair(n, "corprole") == Some((PyRep::Integer(fitting_mgr_role as i64), PyRep::Integer((fitting_mgr_role | delayed_hangar_role) as i64)))),
            "Maturing 24h delayed role grant must emit OnSessionChanged with updated corprole"
        );

        let _ = ctx.drain_notifications_for_char(PILOT_B);
        rpc(
            &ctx,
            &mut sess_a,
            "corpRegistry",
            "UpdateMember",
            &[PyRep::keyval([("characterID", PyRep::Integer(PILOT_B)), ("roles", PyRep::Integer(0)), ("rolesAtHQ", PyRep::Integer(0))])],
        )
        .await
        .expect("UpdateMember revoke");
        let revoke_notifs = ctx.drain_notifications_for_char(PILOT_B);
        let revoke_sc = find_notifs(&revoke_notifs, "OnSessionChanged");
        assert!(revoke_sc.iter().any(|n| n.id_type == "charid"
            && session_change_pair(n, "corprole") == Some((PyRep::Integer((fitting_mgr_role | delayed_hangar_role) as i64), PyRep::Integer(0)))
            && session_change_pair(n, "rolesAtHQ") == Some((PyRep::Integer(hq_role as i64), PyRep::Integer(0)))));
        assert!(
            rpc(&ctx, &mut sess_b, "corpFittingMgr", "SaveFitting", &[PyRep::Integer(player_corp_id), corp_fit])
                .await
                .is_err()
        );
        assert_eq!(sess_b.corp_role, 0);
    }

    #[tokio::test]
    async fn test_medical_clone_grades_removed_free_respawn_zero_sp_loss() {
        // parity: feat:removed.medical-clone-grades
        eve_testkit::attest(&["feat:removed.medical-clone-grades"]);

        let ctx = ServerContext::new();
        let mut sess = login_pilot(&ctx, 8905, "demo", "demo-secret", PILOT_A).await;

        // Seed a 52,000,000 SP veteran pilot (far above pre-Rhea 900,000 SP basic medical clone cap)
        let initial_balance = IskCents(25_000_000_00);
        let initial_sp = 52_000_000.0_f64;
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&PILOT_A) {
                ch.balance = initial_balance;
                ch.skill_points = initial_sp;
                ch.home_station_id = JITA_STATION;
                ch.station_id = Some(JITA_STATION);
            }
            let smap = snap.skills.entry(PILOT_A).or_default();
            smap.insert(3300, TrainedSkillRecord { type_id: 3300, level: 5, skill_points: 256_000.0 });
            smap.insert(3332, TrainedSkillRecord { type_id: 3332, level: 5, skill_points: 512_000.0 });
        });

        // V24 jumpCloneSvc.GetCloneState exposes jump clones, implants, and cooldown — no medical clone grade/SP cap
        let clone_state = rpc(&ctx, &mut sess, "jumpCloneSvc", "GetCloneState", &[])
            .await
            .expect("GetCloneState");
        assert!(clone_state.dict_get("clones").is_some());
        assert!(clone_state.dict_get("cloneGrade").is_none());

        // Negative: home_station.self_destruct_clone while boarded in a non-capsule ship fails
        let non_pod_err = rpc(&ctx, &mut sess, "home_station", "self_destruct_clone", &[])
            .await
            .expect_err("self_destruct_clone requires capsule");
        assert!(!non_pod_err.code.is_empty());

        // Leave ship into Capsule (670) and trigger medical clone activation at home station
        ctx.advance_secs(12);
        let capsule_id = rpc(&ctx, &mut sess, "ship", "LeaveShip", &[])
            .await
            .expect("LeaveShip")
            .as_i64()
            .expect("capsule id");
        assert_eq!(sess.ship_id, Some(capsule_id));
        let _ = ctx.drain_notifications_for_char(PILOT_A);

        let respawn_station = rpc(&ctx, &mut sess, "home_station", "self_destruct_clone", &[])
            .await
            .expect("medical clone activation")
            .as_i64()
            .expect("home station id");
        assert_eq!(respawn_station, JITA_STATION);
        assert_eq!(find_notifs(&ctx.drain_notifications_for_char(PILOT_A), "OnCloneDestruction").len(), 1);

        // Verify 0 ISK fee and 100% SP + level-V skill preservation
        let sheet = rpc(&ctx, &mut sess, "charMgr", "GetCharacterInfo", &[])
            .await
            .expect("GetCharacterInfo after medical clone respawn");
        assert_eq!(
            ctx.db.with_snapshot(|s| s.characters.get(&PILOT_A).map(|c| c.balance)),
            Some(initial_balance),
            "Medical clone respawn must be completely free (0 ISK charged)"
        );
        assert_eq!(
            sheet.dict_get("skillPoints").and_then(|v| v.as_f64()),
            Some(initial_sp),
            "Medical clone respawn must preserve 100% of character SP without pre-Rhea grade loss"
        );
        let (lvl_3300, lvl_3332) = ctx.db.with_snapshot(|s| {
            let smap = s.skills.get(&PILOT_A).expect("skills map");
            (smap[&3300].level, smap[&3332].level)
        });
        assert_eq!((lvl_3300, lvl_3332), (5, 5));
    }

    #[tokio::test]
    async fn test_eiv_market_adjusted_prices_and_warable_lookup_regressions() {
        let ctx = ServerContext::new();
        let mut sess = login_pilot(&ctx, 8906, "demo", "demo-secret", PILOT_A).await;

        let bp = ctx.sde.blueprints.get(&687).cloned().expect("Rifter BP");
        let baseline_eiv = ctx.db.with_snapshot(|snap| {
            eve_server::services::industry_pi::industry_facilities_bps::compute_blueprint_eiv_per_run_with_snap(&ctx, Some(snap), &bp, 1)
        });
        ctx.db.with_mut_snapshot(|snap| {
            snap.market_history.insert(
                (10_000_002, 34),
                (1..=10)
                    .map(|d| MarketHistoryDayRecord {
                        date: format!("2026-10-{d:02}"),
                        average: 25.0,
                        highest: 26.0,
                        lowest: 24.0,
                        order_count: 100,
                        volume: 1_000_000,
                        open_price: None,
                        close_price: None,
                    })
                    .collect(),
            );
        });
        let elevated_eiv = ctx.db.with_snapshot(|snap| {
            eve_server::services::industry_pi::industry_facilities_bps::compute_blueprint_eiv_per_run_with_snap(&ctx, Some(snap), &bp, 1)
        });
        assert!(elevated_eiv > baseline_eiv);

        sess.corporation_id = Some(98_000_001);
        sess.alliance_id = Some(99_000_001);
        ctx.db.with_mut_snapshot(|snap| {
            snap.corporations.insert(
                98_600_001,
                CorporationRecord {
                    corporation_id: 98_600_001,
                    name: "Standalone WarCitadel Corp".into(),
                    ticker: "SWCC".into(),
                    ceo_id: 90_000_050,
                    alliance_id: None,
                    ..Default::default()
                },
            );
            snap.corporations.insert(
                98_600_002,
                CorporationRecord {
                    corporation_id: 98_600_002,
                    name: "Allied WarCitadel Member Corp".into(),
                    ticker: "AWMC".into(),
                    ceo_id: 90_000_051,
                    alliance_id: Some(99_600_002),
                    ..Default::default()
                },
            );
            for (sid, owner) in [(1_000_600_001_001_i64, 98_600_001_i64), (1_000_600_002_001_i64, 98_600_002_i64)] {
                snap.structures.insert(
                    sid,
                    UpwellStructureRecord {
                        structure_id: sid,
                        type_id: 35832,
                        owner_corp_id: owner,
                        solar_system_id: JITA_SYSTEM,
                        name: "WarCitadel Astrahus".into(),
                        position: [0.0, 0.0, 0.0],
                        state: "ShieldVulnerable".into(),
                        quantum_core_installed: true,
                        fuel_blocks: 100,
                        shield_hp: 100.0,
                        armor_hp: 100.0,
                        hull_hp: 100.0,
                        reinforced_until_unix: 0,
                        last_damage_tick: 0,
                        last_damage_unix: 0,
                        self_destruct_at_unix: None,
                    },
                );
            }
            snap.alliances.insert(
                99_600_002,
                AllianceRecord {
                    alliance_id: 99_600_002,
                    name: "WarCitadel Target Alliance".into(),
                    short_name: "WCTA".into(),
                    executor_corp_id: 98_600_002,
                    member_corp_ids: [98_600_002].into_iter().collect(),
                },
            );
        });

        let PyRep::List(rows) = rpc(
            &ctx,
            &mut sess,
            "lookupSvc",
            "LookupWarableCorporationsOrAlliances",
            &[PyRep::String("WarCitadel".into()), PyRep::Bool(false)],
        )
        .await
        .expect("LookupWarableCorporationsOrAlliances") else {
            panic!("expected List");
        };
        let returned_ids: Vec<i64> = rows.iter().filter_map(|r| r.dict_get("ownerID").and_then(|v| v.as_i64())).collect();
        assert!(returned_ids.contains(&98_600_001));
        assert!(returned_ids.contains(&99_600_002));
        assert!(!returned_ids.contains(&98_600_002));
    }

    #[tokio::test]
    async fn test_industry_container_division_and_reactor_gates() {
        let ctx = ServerContext::new();
        let mut sess = login_pilot(&ctx, 8911, "demo", "demo-secret", PILOT_A).await;
        let corp_id = 98_700_001_i64;
        let fm_sag1_only = eve_db::corp_roles::ROLE_FACTORY_MANAGER | (1u64 << 13);
        ctx.db.with_mut_snapshot(|snap| {
            let mut corp = eve_db::CorporationRecord {
                corporation_id: corp_id, name: "Gate Corp".into(), ticker: "GATE".into(),
                ceo_id: PILOT_B, hq_station_id: Some(JITA_STATION), master_wallet: IskCents(100_000_000_00), ..Default::default()
            };
            corp.members.insert(PILOT_A);
            corp.member_roles.insert(PILOT_A, fm_sag1_only);
            snap.corporations.insert(corp_id, corp);
            if let Some(ch) = snap.characters.get_mut(&PILOT_A) { ch.corporation_id = corp_id; ch.balance = IskCents(100_000_000_00); }
            snap.items.insert(931_000_001, make_item(931_000_001, 687, PILOT_A, JITA_STATION, InventoryFlag::Hangar, 1, true));
            snap.items.insert(931_000_002, make_item(931_000_002, 3293, PILOT_A, 60003761, InventoryFlag::Hangar, 1, true));
            snap.items.insert(931_000_003, make_item(931_000_003, 687, corp_id, JITA_STATION, InventoryFlag::CorpSAG2, 1, true));
            snap.structures.insert(931_000_010, crate::mining_operations_and_ledgers::make_structure(931_000_010, corp_id, 35835, 30002813, "Lowsec Athanor", [0.0, 0.0, 0.0]));
            snap.items.insert(931_000_011, make_item(931_000_011, 35891, corp_id, 931_000_010, InventoryFlag::ServiceSlot0, 1, true));
            snap.items.insert(931_000_012, make_item(931_000_012, 90274, PILOT_A, 931_000_010, InventoryFlag::Hangar, 1, true));
        });
        sess.corporation_id = Some(corp_id);
        sess.corp_role = fm_sag1_only;

        let remote_err = rpc(&ctx, &mut sess, "industryManager", "InstallJob", &[PyRep::keyval([
            ("blueprintItemID", PyRep::Integer(931_000_001)), ("facilityID", PyRep::Integer(JITA_STATION)),
            ("activityID", PyRep::Integer(1)), ("runs", PyRep::Integer(1)), ("inputContainerID", PyRep::Integer(931_000_002)),
        ])]).await.expect_err("remote inputContainerID must fail");
        assert_eq!(remote_err.code, "InvalidInputContainer");

        let div_err = rpc(&ctx, &mut sess, "industryManager", "InstallJob", &[PyRep::keyval([
            ("blueprintItemID", PyRep::Integer(931_000_003)), ("facilityID", PyRep::Integer(JITA_STATION)),
            ("activityID", PyRep::Integer(1)), ("runs", PyRep::Integer(1)),
            ("useCorp", PyRep::Bool(true)), ("inputFlag", PyRep::Integer(InventoryFlag::CorpSAG2 as i64)),
        ])]).await.expect_err("unauthorized CorpSAG2 must fail");
        assert_eq!(div_err.code, "CrpAccessDenied");

        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&PILOT_A) {
                ch.station_id = None; ch.structure_id = Some(931_000_010); ch.solar_system_id = 30002813;
            }
        });
        sess.station_id = None; sess.structure_id = Some(931_000_010); sess.solar_system_id = Some(30002813);
        let react_err = rpc(&ctx, &mut sess, "industryManager", "InstallJob", &[PyRep::keyval([
            ("blueprintItemID", PyRep::Integer(931_000_012)), ("facilityID", PyRep::Integer(931_000_010)),
            ("activityID", PyRep::Integer(9)), ("runs", PyRep::Integer(1)),
        ])]).await.expect_err("35891 Standup Research Lab I must be rejected for composite reaction");
        assert_eq!(react_err.code, "CompositeReactorRequired");
    }
}
