//! Integration tests for Fighter Squadrons & Carrier Operations:
//! - feat:drones.fighter-heavy-long-range-bomb
//! - feat:drones.fighter-refuel-rearm-repair
//! - feat:drones.fighter-support-cenotaph-scarab
//! - feat:drones.light-fighter-space-superiority
//! - feat:drones.support-fighter-abilities

#![forbid(unsafe_code)]

use crate::common;
use eve_db::types::{InventoryFlag, InventoryItemRecord, IskCents};
use eve_destiny::{Ball, DamageVector, MovementMode, TargetLock};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::context::ActiveFighterSquadron;
use eve_server::router::ServerContext;
use eve_server::services::combat_crimewatch::{apply_damage_to_fighter_squadron, with_combat_runtime};
use glam::DVec3;
use std::collections::HashMap;

const AEON_SUPERCARRIER_TYPE_ID: u32 = 23919; // Aeon Supercarrier (Heavy fighter tubes)
const ARCHON_CARRIER_TYPE_ID: u32 = 23757; // Archon Carrier (Light & Support tubes)
const AMETAT_HEAVY_FIGHTER_TYPE_ID: u32 = 40362; // Ametat I Long-Range Heavy Fighter (AoE Bomb)
const SIREN_SUPPORT_FIGHTER_TYPE_ID: u32 = 40346; // Siren I Support Fighter (Warp Disruption)
const DROMI_SUPPORT_FIGHTER_TYPE_ID: u32 = 40347; // Dromi I Support Fighter (Webification)
const CENOBITE_SUPPORT_FIGHTER_TYPE_ID: u32 = 37599; // Cenobite I Support Fighter (Energy Neutralization)
const SCARAB_SUPPORT_FIGHTER_TYPE_ID: u32 = 40345; // Scarab I Support Fighter (ECM Jammer)
const EQUITE_SPACE_SUPERIORITY_TYPE_ID: u32 = 40358; // Equite I Space Superiority Fighter
const GRAM_SPACE_SUPERIORITY_TYPE_ID: u32 = 40361; // Gram I Space Superiority Fighter
const LOCUST_SPACE_SUPERIORITY_TYPE_ID: u32 = 40359; // Locust I Space Superiority Fighter
const SATYR_SPACE_SUPERIORITY_TYPE_ID: u32 = 40360; // Satyr I Space Superiority Fighter
const VEXOR_CRUISER_TYPE_ID: u32 = 626;

fn make_item(
    item_id: i64,
    type_id: u32,
    owner_id: i64,
    location_id: i64,
    flag: InventoryFlag,
    quantity: i32,
) -> InventoryItemRecord {
    InventoryItemRecord {
        item_id,
        type_id,
        owner_id,
        location_id,
        flag,
        quantity,
        singleton: true,
        version: 1,
        ..Default::default()
    }
}

async fn spawn_ship_ball(
    ctx: &ServerContext,
    sys_id: u32,
    id: i64,
    type_id: u32,
    pos: DVec3,
    max_hp: f64,
) {
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let mut cs = ctx.build_type_combat_state(type_id);
    cs.shield_hp = max_hp;
    cs.max_shield_hp = max_hp;
    cs.armor_hp = max_hp;
    cs.max_armor_hp = max_hp;
    cs.structure_hp = max_hp;
    cs.max_structure_hp = max_hp;
    cs.capacitor = 1_000.0;
    cs.max_capacitor = 1_000.0;
    let _ = sys_handle
        .spawn_ball_with_combat(
            Ball {
                id,
                type_id,
                position: pos,
                velocity: DVec3::ZERO,
                max_velocity: 350.0,
                inertia: 2.0,
                mass: 1_200_000.0,
                radius: 50.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            cs,
        )
        .await;
}

async fn setup_pilot_session(
    ctx: &ServerContext,
    session_id: u64,
    char_id: i64,
    ship_id: i64,
    ship_type_id: u32,
    sys_id: u32,
) -> SessionState {
    let kw = HashMap::new();
    let mut sess = SessionState::new(session_id);
    let (user, pass) = if char_id == 90_000_002 {
        ("other", "other-secret")
    } else {
        ("demo", "demo-secret")
    };
    common::login_pilot(ctx, &mut sess, user, pass).await;
    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID");

    sess.solar_system_id = Some(sys_id);
    sess.station_id = None;
    sess.structure_id = None;
    sess.ship_id = Some(ship_id);

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = sys_id;
            ch.active_ship_id = ship_id;
            ch.station_id = None;
            ch.balance = IskCents::from_isk(500_000_000);
        }
        snap.items.insert(
            ship_id,
            make_item(ship_id, ship_type_id, char_id, sys_id as i64, InventoryFlag::None, 1),
        );
    });
    spawn_ship_ball(ctx, sys_id, ship_id, ship_type_id, DVec3::ZERO, 500_000.0).await;
    sess
}

// parity: feat:drones.fighter-heavy-long-range-bomb, feat:drones.fighter-refuel-rearm-repair
#[tokio::test]
async fn test_fighter_heavy_long_range_bomb_and_tube_rearm_repair() {
    eve_testkit::attest(&[
        "feat:drones.fighter-heavy-long-range-bomb",
        "feat:drones.fighter-refuel-rearm-repair",
    ]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let kw = HashMap::new();

    let pilot_char_id = 90_000_001i64;
    let aeon_ship_id = 104_000_101i64;
    let victim_char_id = 90_000_002i64;
    let victim_ship_id = 104_000_102i64;
    let far_ship_id = 104_000_103i64;

    // 1. Highsec restriction: bomb forbidden in systems with security >= 0.45 (Jita 30000142)
    let highsec_sys = 30000142u32;
    let mut highsec_sess = setup_pilot_session(&ctx, 1001, pilot_char_id, aeon_ship_id, AEON_SUPERCARRIER_TYPE_ID, highsec_sys).await;
    let hs_fighter_item = 310_001i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(hs_fighter_item, make_item(hs_fighter_item, AMETAT_HEAVY_FIGHTER_TYPE_ID, pilot_char_id, aeon_ship_id, InventoryFlag::FighterBay, 6));
    });
    ctx.fighter_tube_loads.write().unwrap().insert((aeon_ship_id, 0), hs_fighter_item);
    let hs_launch = ctx
        .dispatch_rpc(&mut highsec_sess, "fighterMgr", None, "LaunchFightersFromTubes", &[PyRep::List(vec![PyRep::Integer(0)])], &kw, None)
        .await
        .expect("Launch in highsec");
    let PyRep::List(hs_balls) = hs_launch else { panic!() };
    let hs_squad_ball_id = hs_balls[0].as_i64().unwrap();

    let hs_bomb_err = ctx
        .dispatch_rpc(
            &mut highsec_sess,
            "fighterMgr",
            None,
            "ActivateAbilitySlotsAtPoint",
            &[
                PyRep::Integer(hs_squad_ball_id),
                PyRep::Integer(2),
                PyRep::List(vec![PyRep::Real(5_000.0), PyRep::Real(0.0), PyRep::Real(0.0)]),
            ],
            &kw,
            None,
        )
        .await
        .expect_err("Point bomb must be forbidden in high-security space");
    assert_eq!(hs_bomb_err.code, "BombForbiddenInHighSec");

    // Clean up highsec test entities
    ctx.active_fighters.write().unwrap().remove(&hs_squad_ball_id);
    let _ = ctx.systems.get_or_spawn(highsec_sys).remove_ball(hs_squad_ball_id).await;

    // 2. Nullsec deployment: Kuharah (30000021, sec = -1.0)
    let nullsec_sys = 30000021u32;
    let mut nullsec_sess = setup_pilot_session(&ctx, 1002, pilot_char_id, aeon_ship_id, AEON_SUPERCARRIER_TYPE_ID, nullsec_sys).await;
    let _victim_sess = setup_pilot_session(&ctx, 1003, victim_char_id, victim_ship_id, VEXOR_CRUISER_TYPE_ID, nullsec_sys).await;

    let squad_item_id = 310_002i64;
    let reserve_item_id = 310_003i64;
    ctx.db.with_mut_snapshot(|snap| {
        // Squadron of 6 Ametat I loaded in tube 0
        snap.items.insert(squad_item_id, make_item(squad_item_id, AMETAT_HEAVY_FIGHTER_TYPE_ID, pilot_char_id, aeon_ship_id, InventoryFlag::FighterBay, 6));
        // Reserve stack of 6 Ametat I in FighterBay for tube replacement
        snap.items.insert(reserve_item_id, make_item(reserve_item_id, AMETAT_HEAVY_FIGHTER_TYPE_ID, pilot_char_id, aeon_ship_id, InventoryFlag::FighterBay, 6));
    });
    ctx.fighter_tube_loads.write().unwrap().insert((aeon_ship_id, 0), squad_item_id);

    let launch_res = ctx
        .dispatch_rpc(&mut nullsec_sess, "fighterMgr", None, "LaunchFightersFromTubes", &[PyRep::List(vec![PyRep::Integer(0)])], &kw, None)
        .await
        .expect("Launch Ametat I in nullsec");
    let PyRep::List(launched_balls) = launch_res else { panic!() };
    let squad_ball_id = launched_balls[0].as_i64().unwrap();

    // Spawn victim cruiser at [5000, 0, 0] (inside AoE radius 15,000m)
    let target_point = DVec3::new(5_000.0, 0.0, 0.0);
    spawn_ship_ball(&ctx, nullsec_sys, victim_ship_id, VEXOR_CRUISER_TYPE_ID, target_point, 10_000.0).await;
    // Spawn far cruiser at [40000, 0, 0] (outside AoE radius)
    spawn_ship_ball(&ctx, nullsec_sys, far_ship_id, VEXOR_CRUISER_TYPE_ID, DVec3::new(40_000.0, 0.0, 0.0), 10_000.0).await;

    // Out-of-range check (> 100,000 m)
    let range_err = ctx
        .dispatch_rpc(
            &mut nullsec_sess,
            "fighterMgr",
            None,
            "CmdActivateAbilitySlots",
            &[
                PyRep::Integer(squad_ball_id),
                PyRep::Integer(2),
                PyRep::List(vec![PyRep::Real(150_000.0), PyRep::Real(0.0), PyRep::Real(0.0)]),
            ],
            &kw,
            None,
        )
        .await
        .expect_err("Target point > 100km must fail TargetOutOfRange");
    assert_eq!(range_err.code, "TargetOutOfRange");

    // Fire point-targeted AoE bomb ability via ActivateAbilitySlotsAtPoint
    let bomb_res = ctx
        .dispatch_rpc(
            &mut nullsec_sess,
            "fighterMgr",
            None,
            "ActivateAbilitySlotsAtPoint",
            &[
                PyRep::Integer(squad_ball_id),
                PyRep::Integer(2), // slot 2: Long-range heavy bomb
                PyRep::List(vec![PyRep::Real(target_point.x), PyRep::Real(target_point.y), PyRep::Real(target_point.z)]),
            ],
            &kw,
            None,
        )
        .await
        .expect("ActivateAbilitySlotsAtPoint in nullsec");
    let PyRep::KeyVal(bomb_dict) = bomb_res else { panic!("expected KeyVal") };
    assert_eq!(bomb_dict.get("success"), Some(&PyRep::Bool(true)));
    assert_eq!(bomb_dict.get("remainingCharges").and_then(|v| v.as_i64()), Some(2)); // Started at 3, consumed 1
    assert_eq!(bomb_dict.get("pointTargeted"), Some(&PyRep::Bool(true)));
    assert_eq!(
        bomb_dict.get("detonationPoint"),
        Some(&PyRep::Tuple(vec![PyRep::Real(target_point.x), PyRep::Real(target_point.y), PyRep::Real(target_point.z)]))
    );

    // Verify ability cooldown
    let cd_err = ctx
        .dispatch_rpc(
            &mut nullsec_sess,
            "fighterMgr",
            None,
            "ActivateAbilitySlotsAtPoint",
            &[
                PyRep::Integer(squad_ball_id),
                PyRep::Integer(2),
                PyRep::List(vec![PyRep::Real(target_point.x), PyRep::Real(target_point.y), PyRep::Real(target_point.z)]),
            ],
            &kw,
            None,
        )
        .await
        .expect_err("Must fail AbilityOnCooldown");
    assert_eq!(cd_err.code, "AbilityOnCooldown");

    // Verify AoE damage to victim ship and zero damage to far ship
    let null_sys = ctx.systems.get_or_spawn(nullsec_sys);
    let victim_cs = null_sys.get_combat_state(victim_ship_id).await.unwrap().unwrap();
    assert!(victim_cs.shield_hp < 10_000.0, "Victim in AoE radius must take bomb damage");
    let far_cs = null_sys.get_combat_state(far_ship_id).await.unwrap().unwrap();
    assert_eq!(far_cs.shield_hp, 10_000.0, "Ship outside AoE radius must take 0 damage");

    // Advance past cooldown and test CmdActivateAbilitySlots also triggers point bomb
    ctx.advance_secs(65);
    let bomb2_res = ctx
        .dispatch_rpc(
            &mut nullsec_sess,
            "fighterMgr",
            None,
            "CmdActivateAbilitySlots",
            &[
                PyRep::Integer(squad_ball_id),
                PyRep::Integer(2),
                PyRep::Tuple(vec![PyRep::Real(target_point.x), PyRep::Real(target_point.y), PyRep::Real(target_point.z)]),
            ],
            &kw,
            None,
        )
        .await
        .expect("CmdActivateAbilitySlots with point coordinates");
    let PyRep::KeyVal(bomb2_dict) = bomb2_res else { panic!("expected KeyVal") };
    assert_eq!(bomb2_dict.get("remainingCharges").and_then(|v| v.as_i64()), Some(1));

    // 3. Fighter refuel, rearm, repair & squadron replacement
    // Damage the squadron and destroy 1 fighter (squadron size drops from 6 to 5)
    let (lost, remaining_size, wiped_out) = apply_damage_to_fighter_squadron(
        &ctx,
        nullsec_sys,
        squad_ball_id,
        DamageVector::new(4_000.0, 0.0, 0.0, 0.0),
    )
    .await;
    assert_eq!(lost, 1, "Expected 1 fighter unit destroyed");
    assert_eq!(remaining_size, 5, "Expected 5 fighters remaining in squadron");
    assert!(!wiped_out);
    assert_eq!(ctx.active_fighters.read().unwrap().get(&squad_ball_id).unwrap().squadron_size, 5);

    // Recall fighters to tube 0
    let recall_res = ctx
        .dispatch_rpc(
            &mut nullsec_sess,
            "fighterMgr",
            None,
            "RecallFightersToTubes",
            &[PyRep::List(vec![PyRep::Integer(squad_ball_id)])],
            &kw,
            None,
        )
        .await
        .expect("RecallFightersToTubes");
    let PyRep::List(recalled) = recall_res else { panic!() };
    assert_eq!(recalled.len(), 1);

    // Verify tube rearm/refueling timer is active (15 seconds)
    let is_rearming = with_combat_runtime(&ctx, |rt| {
        rt.fighter_tube_rearm_until.get(&(aeon_ship_id, 0)).copied().unwrap_or(0) > ctx.now_unix()
    });
    assert!(is_rearming, "Recalled tube must have rearm timer active");

    // Verify destroyed fighters were replaced from the FighterBay reserve stack
    let (squad_qty, reserve_qty) = ctx.db.with_snapshot(|snap| {
        (snap.items.get(&squad_item_id).unwrap().quantity, snap.items.get(&reserve_item_id).unwrap().quantity)
    });
    assert_eq!(squad_qty, 6, "Squadron size restored to full capacity (6) from bay reserve");
    assert_eq!(reserve_qty, 5, "Reserve stack consumed 1 fighter to replace destroyed unit");

    // Immediate relaunch must fail with TubeRearming
    let relaunch_err = ctx
        .dispatch_rpc(&mut nullsec_sess, "fighterMgr", None, "LaunchFightersFromTubes", &[PyRep::List(vec![PyRep::Integer(0)])], &kw, None)
        .await
        .expect_err("Launch while rearming must fail");
    assert_eq!(relaunch_err.code, "TubeRearming");

    // Advance clock past SDE per-charge bomb rearm + replacement + refuel timer
    ctx.advance_secs(50);

    // Relaunch succeeds
    let relaunch_ok = ctx
        .dispatch_rpc(&mut nullsec_sess, "fighterMgr", None, "LaunchFightersFromTubes", &[PyRep::List(vec![PyRep::Integer(0)])], &kw, None)
        .await
        .expect("Relaunch after rearm timer expires");
    let PyRep::List(relaunched_balls) = relaunch_ok else { panic!() };
    let new_squad_ball_id = relaunched_balls[0].as_i64().unwrap();

    // Verify HP fully repaired
    let new_cs = null_sys.get_combat_state(new_squad_ball_id).await.unwrap().unwrap();
    assert_eq!(new_cs.shield_hp, new_cs.max_shield_hp, "Fighter shield HP fully repaired");
    assert_eq!(new_cs.armor_hp, new_cs.max_armor_hp, "Fighter armor HP fully repaired");

    // Verify depleted ability charges reloaded (can fire bomb again with full charges)
    let rearmed_bomb = ctx
        .dispatch_rpc(
            &mut nullsec_sess,
            "fighterMgr",
            None,
            "ActivateAbilitySlotsAtPoint",
            &[
                PyRep::Integer(new_squad_ball_id),
                PyRep::Integer(2),
                PyRep::List(vec![PyRep::Real(target_point.x), PyRep::Real(target_point.y), PyRep::Real(target_point.z)]),
            ],
            &kw,
            None,
        )
        .await
        .expect("Bomb fired after rearm");
    let PyRep::KeyVal(rearmed_dict) = rearmed_bomb else { panic!() };
    assert_eq!(rearmed_dict.get("remainingCharges").and_then(|v| v.as_i64()), Some(2)); // Started at 3 fresh charges
}

// parity: feat:drones.fighter-support-cenotaph-scarab, feat:drones.support-fighter-abilities, feat:drones.light-fighter-space-superiority
#[tokio::test]
async fn test_support_fighter_abilities_and_space_superiority() {
    eve_testkit::attest(&[
        "feat:drones.fighter-support-cenotaph-scarab",
        "feat:drones.support-fighter-abilities",
        "feat:drones.light-fighter-space-superiority",
    ]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let kw = HashMap::new();

    let pilot_char_id = 90_000_001i64;
    let archon_ship_id = 104_000_201i64;
    let target_char_id = 90_000_002i64;
    let target_ship_id = 104_000_202i64;
    let lowsec_sys = 30002187u32; // Lowsec system (sec = 0.4)

    let mut pilot_sess = setup_pilot_session(&ctx, 2001, pilot_char_id, archon_ship_id, ARCHON_CARRIER_TYPE_ID, lowsec_sys).await;
    let _target_sess = setup_pilot_session(&ctx, 2002, target_char_id, target_ship_id, VEXOR_CRUISER_TYPE_ID, lowsec_sys).await;

    // Archon Carrier has 2 support fighter tube slots (attr 2218 == 2).
    // Test Pair 1: Siren I (Warp Disruption) and Dromi I (Webification)
    let siren_item = 310_011i64;
    let dromi_item = 310_012i64;
    let cenobite_item = 310_013i64;
    let scarab_item = 310_014i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(siren_item, make_item(siren_item, SIREN_SUPPORT_FIGHTER_TYPE_ID, pilot_char_id, archon_ship_id, InventoryFlag::FighterBay, 3));
        snap.items.insert(dromi_item, make_item(dromi_item, DROMI_SUPPORT_FIGHTER_TYPE_ID, pilot_char_id, archon_ship_id, InventoryFlag::FighterBay, 3));
        snap.items.insert(cenobite_item, make_item(cenobite_item, CENOBITE_SUPPORT_FIGHTER_TYPE_ID, pilot_char_id, archon_ship_id, InventoryFlag::FighterBay, 3));
        snap.items.insert(scarab_item, make_item(scarab_item, SCARAB_SUPPORT_FIGHTER_TYPE_ID, pilot_char_id, archon_ship_id, InventoryFlag::FighterBay, 3));
    });
    {
        let mut loads = ctx.fighter_tube_loads.write().unwrap();
        loads.insert((archon_ship_id, 0), siren_item);
        loads.insert((archon_ship_id, 1), dromi_item);
    }

    let launch_pair1 = ctx
        .dispatch_rpc(&mut pilot_sess, "fighterMgr", None, "LaunchFightersFromTubes", &[PyRep::List(vec![PyRep::Integer(0), PyRep::Integer(1)])], &kw, None)
        .await
        .expect("Launch Siren and Dromi");
    let PyRep::List(pair1_balls) = launch_pair1 else { panic!() };
    let (siren_ball, dromi_ball) = (pair1_balls[0].as_i64().unwrap(), pair1_balls[1].as_i64().unwrap());

    let sys_handle = ctx.systems.get_or_spawn(lowsec_sys);
    spawn_ship_ball(&ctx, lowsec_sys, target_ship_id, VEXOR_CRUISER_TYPE_ID, DVec3::new(1000.0, 0.0, 0.0), 10_000.0).await;
    sys_handle
        .add_target_lock(
            archon_ship_id,
            TargetLock {
                target_id: target_ship_id,
                lock_time_ms: 1000.0,
                remaining_lock_ms: 0.0,
                locked: true,
                max_range_m: None,
            },
        )
        .await
        .unwrap();

    // 1. Siren Warp Disruption scaling with squadron size
    // Full squadron (size 3) -> scram strength = 3 points
    ctx.dispatch_rpc(&mut pilot_sess, "fighterMgr", None, "CmdActivateAbilitySlots", &[PyRep::Integer(siren_ball), PyRep::Integer(0), PyRep::Integer(target_ship_id)], &kw, None)
        .await
        .expect("Siren Warp Disruption");
    let scram_full = sys_handle.get_combat_state(target_ship_id).await.unwrap().unwrap().warp_scramble_strength;
    assert_eq!(scram_full, 3, "Siren squadron of 3 must apply 3 warp disruption points");

    // Reduce Siren squadron size to 1 -> scram strength = 1 point
    ctx.active_fighters.write().unwrap().get_mut(&siren_ball).unwrap().squadron_size = 1;
    ctx.advance_secs(15);
    ctx.dispatch_rpc(&mut pilot_sess, "fighterMgr", None, "CmdActivateAbilitySlots", &[PyRep::Integer(siren_ball), PyRep::Integer(0), PyRep::Integer(target_ship_id)], &kw, None)
        .await
        .expect("Siren Warp Disruption size 1");
    let scram_reduced = sys_handle.get_combat_state(target_ship_id).await.unwrap().unwrap().warp_scramble_strength;
    assert_eq!(scram_reduced, 1, "Siren squadron of 1 must apply 1 warp disruption point (scaled by surviving size)");

    // 2. Dromi Stasis Webification scaling with squadron size
    // Full squadron (size 3)
    ctx.dispatch_rpc(&mut pilot_sess, "fighterMgr", None, "CmdActivateAbilitySlots", &[PyRep::Integer(dromi_ball), PyRep::Integer(0), PyRep::Integer(target_ship_id)], &kw, None)
        .await
        .expect("Dromi Webification");
    let speed_full_web = sys_handle.get_ball_snapshot(target_ship_id).await.unwrap().unwrap().1.speed_fraction;
    assert!(speed_full_web <= 0.75, "Full Dromi squadron must apply strong web reduction");

    // Reduce Dromi squadron size to 1 -> weaker web
    ctx.active_fighters.write().unwrap().get_mut(&dromi_ball).unwrap().squadron_size = 1;
    ctx.advance_secs(15);
    ctx.dispatch_rpc(&mut pilot_sess, "fighterMgr", None, "CmdActivateAbilitySlots", &[PyRep::Integer(dromi_ball), PyRep::Integer(0), PyRep::Integer(target_ship_id)], &kw, None)
        .await
        .expect("Dromi Webification size 1");
    let speed_weak_web = sys_handle.get_ball_snapshot(target_ship_id).await.unwrap().unwrap().1.speed_fraction;
    assert!(speed_weak_web > speed_full_web, "Reduced Dromi squadron size must apply weaker web factor");

    // Recall Pair 1
    let _ = ctx.dispatch_rpc(&mut pilot_sess, "fighterMgr", None, "RecallFightersToTubes", &[PyRep::List(vec![PyRep::Integer(siren_ball), PyRep::Integer(dromi_ball)])], &kw, None).await.unwrap();
    ctx.advance_secs(15);

    // Test Pair 2: Cenobite I (Energy Neutralization) and Scarab I (ECM Jamming)
    {
        let mut loads = ctx.fighter_tube_loads.write().unwrap();
        loads.insert((archon_ship_id, 0), cenobite_item);
        loads.insert((archon_ship_id, 1), scarab_item);
    }
    let launch_pair2 = ctx
        .dispatch_rpc(&mut pilot_sess, "fighterMgr", None, "LaunchFightersFromTubes", &[PyRep::List(vec![PyRep::Integer(0), PyRep::Integer(1)])], &kw, None)
        .await
        .expect("Launch Cenobite and Scarab");
    let PyRep::List(pair2_balls) = launch_pair2 else { panic!() };
    let (cenobite_ball, scarab_ball) = (pair2_balls[0].as_i64().unwrap(), pair2_balls[1].as_i64().unwrap());

    // 3. Cenobite Energy Neutralization: drains capacitor scaled by squadron size
    let cap_before = sys_handle.get_combat_state(target_ship_id).await.unwrap().unwrap().capacitor;
    ctx.dispatch_rpc(&mut pilot_sess, "fighterMgr", None, "CmdActivateAbilitySlots", &[PyRep::Integer(cenobite_ball), PyRep::Integer(0), PyRep::Integer(target_ship_id)], &kw, None)
        .await
        .expect("Cenobite Energy Neutralization");
    let cap_after = sys_handle.get_combat_state(target_ship_id).await.unwrap().unwrap().capacitor;
    assert!(cap_after < cap_before, "Cenobite energy neutralization must drain target capacitor");

    // 4. Scarab ECM Jamming: breaks target locks when jam strength exceeds threshold
    sys_handle
        .add_target_lock(
            target_ship_id,
            TargetLock {
                target_id: archon_ship_id,
                lock_time_ms: 1000.0,
                remaining_lock_ms: 0.0,
                locked: true,
                max_range_m: None,
            },
        )
        .await
        .unwrap();
    assert!(
        sys_handle
            .get_target_locks(target_ship_id)
            .await
            .unwrap()
            .iter()
            .any(|l| l.target_id == archon_ship_id),
        "Target locked carrier"
    );
    ctx.dispatch_rpc(&mut pilot_sess, "fighterMgr", None, "CmdActivateAbilitySlots", &[PyRep::Integer(scarab_ball), PyRep::Integer(0), PyRep::Integer(target_ship_id)], &kw, None)
        .await
        .expect("Scarab ECM Jamming");
    assert!(
        !sys_handle
            .get_target_locks(target_ship_id)
            .await
            .unwrap()
            .iter()
            .any(|l| l.target_id == archon_ship_id),
        "Scarab ECM must break target locks"
    );

    // Recall Pair 2
    let _ = ctx.dispatch_rpc(&mut pilot_sess, "fighterMgr", None, "RecallFightersToTubes", &[PyRep::List(vec![PyRep::Integer(cenobite_ball), PyRep::Integer(scarab_ball)])], &kw, None).await.unwrap();
    ctx.advance_secs(15);

    // 5. Space Superiority Light Fighters: Equite, Gram, Locust, Satyr
    // Verify all 4 space superiority fighter hulls exist in SDE with Evasive Maneuvers (slot 1) and Anti-Fighter Volley (slot 2)
    for tid in [
        EQUITE_SPACE_SUPERIORITY_TYPE_ID,
        GRAM_SPACE_SUPERIORITY_TYPE_ID,
        LOCUST_SPACE_SUPERIORITY_TYPE_ID,
        SATYR_SPACE_SUPERIORITY_TYPE_ID,
    ] {
        let slots = ctx.sde.fighter_abilities_by_type.get(&tid).expect("Space superiority abilities");
        assert!(slots.iter().any(|s| s.slot_index == 1 && s.ability_id == 13), "Slot 1 must be Evasive Maneuvers (ability 13)");
        assert!(slots.iter().any(|s| s.slot_index == 2 && s.ability_id == 16), "Slot 2 must be Anti-Fighter Missile Volley (ability 16)");
    }

    // Recall support fighters and load Equite I
    let _ = ctx.dispatch_rpc(&mut pilot_sess, "fighterMgr", None, "RecallFightersToTubes", &[PyRep::List(vec![PyRep::Integer(siren_ball), PyRep::Integer(dromi_ball), PyRep::Integer(cenobite_ball), PyRep::Integer(scarab_ball)])], &kw, None).await.unwrap();
    ctx.advance_secs(15);

    let equite_item = 310_021i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(equite_item, make_item(equite_item, EQUITE_SPACE_SUPERIORITY_TYPE_ID, pilot_char_id, archon_ship_id, InventoryFlag::FighterBay, 9));
    });
    ctx.fighter_tube_loads.write().unwrap().insert((archon_ship_id, 0), equite_item);

    let equite_launch = ctx.dispatch_rpc(&mut pilot_sess, "fighterMgr", None, "LaunchFightersFromTubes", &[PyRep::List(vec![PyRep::Integer(0)])], &kw, None).await.unwrap();
    let PyRep::List(eq_balls) = equite_launch else { panic!() };
    let equite_ball = eq_balls[0].as_i64().unwrap();

    let initial_b = sys_handle.get_ball_snapshot(equite_ball).await.unwrap().unwrap().1;
    let initial_cs = sys_handle.get_combat_state(equite_ball).await.unwrap().unwrap();

    // Activate Evasive Maneuvers (slot 1, untargeted)
    let evasive_res = ctx
        .dispatch_rpc(&mut pilot_sess, "fighterMgr", None, "CmdActivateAbilitySlots", &[PyRep::Integer(equite_ball), PyRep::Integer(1), PyRep::None], &kw, None)
        .await
        .expect("Activate Evasive Maneuvers");
    let PyRep::KeyVal(evasive_dict) = evasive_res else { panic!() };
    assert_eq!(evasive_dict.get("success"), Some(&PyRep::Bool(true)));
    assert_eq!(evasive_dict.get("cooldownSeconds").and_then(|v| v.as_f64()), Some(60.0));

    // Verify velocity increased (+200% SDE attr 2224) and signature radius reduced by 80% (SDE attr 2225 = -80%)
    let boosted_b = sys_handle.get_ball_snapshot(equite_ball).await.unwrap().unwrap().1;
    let boosted_cs = sys_handle.get_combat_state(equite_ball).await.unwrap().unwrap();
    assert!(boosted_b.max_velocity > initial_b.max_velocity * 2.5, "Evasive Maneuvers must increase fighter velocity (+200% SDE)");
    assert!((boosted_cs.signature_radius - initial_cs.signature_radius * 0.20).abs() < 1e-3, "Evasive Maneuvers must cut signature radius by 80% (SDE attr 2225)");

    // Verify cooldown prevents immediate re-activation
    let eq_cd_err = ctx.dispatch_rpc(&mut pilot_sess, "fighterMgr", None, "CmdActivateAbilitySlots", &[PyRep::Integer(equite_ball), PyRep::Integer(1), PyRep::None], &kw, None).await.unwrap_err();
    assert_eq!(eq_cd_err.code, "AbilityOnCooldown");

    // Spawn an enemy fighter squadron to test anti-fighter missile volley damage bonus
    let enemy_fighter_ball = 620_000_001i64;
    spawn_ship_ball(&ctx, lowsec_sys, enemy_fighter_ball, EQUITE_SPACE_SUPERIORITY_TYPE_ID, DVec3::new(2000.0, 0.0, 0.0), 5_000.0).await;
    ctx.active_fighters.write().unwrap().insert(
        enemy_fighter_ball,
        ActiveFighterSquadron {
            ball_id: enemy_fighter_ball,
            item_id: 310_099,
            type_id: EQUITE_SPACE_SUPERIORITY_TYPE_ID,
            owner_char_id: target_char_id,
            parent_ship_id: target_ship_id,
            tube_index: 0,
            squadron_size: 9,
            target_id: None,
            is_abandoned: false,
            mwd_active: false,
        },
    );

    sys_handle
        .add_target_lock(
            archon_ship_id,
            TargetLock {
                target_id: enemy_fighter_ball,
                lock_time_ms: 1000.0,
                remaining_lock_ms: 0.0,
                locked: true,
                max_range_m: None,
            },
        )
        .await
        .unwrap();

    // Activate Anti-Fighter Missile Volley (slot 2) against enemy fighter squadron (+100% damage bonus)
    let volley_res = ctx
        .dispatch_rpc(&mut pilot_sess, "fighterMgr", None, "CmdActivateAbilitySlots", &[PyRep::Integer(equite_ball), PyRep::Integer(2), PyRep::Integer(enemy_fighter_ball)], &kw, None)
        .await
        .expect("Anti-fighter missile volley");
    let PyRep::KeyVal(v_dict) = volley_res else { panic!() };
    assert_eq!(v_dict.get("success"), Some(&PyRep::Bool(true)));
    let enemy_cs = sys_handle.get_combat_state(enemy_fighter_ball).await.unwrap().unwrap();
    assert!(enemy_cs.shield_hp < 5_000.0, "Enemy fighter squadron must take anti-fighter missile volley damage");
}
