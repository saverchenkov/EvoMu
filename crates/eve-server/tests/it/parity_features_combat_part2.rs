//! Dedicated end-to-end integration tests for `feat:drones.*` fighter parity rows (Part 2).

use eve_db::{InventoryFlag, IskCents};
use eve_destiny::{Ball, DamageVector, DVec3, MovementMode};
use eve_machonet::{service_roles, SessionState};
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::services::combat_crimewatch::{
    apply_damage_to_fighter_squadron, dispatch_combat_rpc, with_combat_runtime,
};
use std::collections::HashMap;

const ARCHON_CARRIER: u32 = 23757;
const AMETAT_HEAVY_FIGHTER: u32 = 40362;
const EQUITE_LIGHT_FIGHTER: u32 = 40358;
const SIREN_SUPPORT_FIGHTER: u32 = 40559;
const DROMI_SUPPORT_FIGHTER: u32 = 40561;
const CENOBITE_SUPPORT_FIGHTER: u32 = 40560;
const SCARAB_SUPPORT_FIGHTER: u32 = 40562;
const SYS_ID: u32 = 30_004_759;

fn make_sess(char_id: i64, ship_id: i64) -> SessionState {
    let mut s = SessionState::new(6001);
    s.user_id = Some(9001);
    s.character_id = Some(char_id);
    s.corporation_id = Some(1_000_009);
    s.solar_system_id = Some(SYS_ID);
    s.constellation_id = Some(20_000_020);
    s.region_id = Some(10_000_002);
    s.ship_id = Some(ship_id);
    s.role = service_roles::ROLE_PLAYER | service_roles::ROLE_LOGIN;
    s
}

async fn setup_carrier(ctx: &ServerContext, char_id: i64, ship_id: i64) {
    crate::open_feats_b35::give_item(ctx, ship_id, ARCHON_CARRIER, char_id, SYS_ID as i64, InventoryFlag::None, 1);
    ctx.db.with_mut_snapshot(|s| {
        if let Some(ch) = s.characters.get_mut(&char_id) {
            ch.solar_system_id = SYS_ID;
            ch.active_ship_id = ship_id;
            ch.balance = IskCents::from_isk(500_000_000);
        }
    });
    let sys = ctx.systems.get_or_spawn(SYS_ID);
    let mut cs = ctx.build_type_combat_state(ARCHON_CARRIER);
    cs.capacitor = 100_000.0;
    cs.max_capacitor = 100_000.0;
    let _ = sys
        .spawn_ball_with_combat(
            Ball {
                id: ship_id,
                type_id: ARCHON_CARRIER,
                position: DVec3::ZERO,
                velocity: DVec3::ZERO,
                max_velocity: 100.0,
                inertia: 1.0,
                mass: 100_000_000.0,
                radius: 400.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            cs,
        )
        .await;
}

async fn setup_target_cruiser(ctx: &ServerContext, char_id: i64, ship_id: i64, pos: DVec3) {
    crate::open_feats_b35::give_item(ctx, ship_id, 626, char_id, SYS_ID as i64, InventoryFlag::None, 1);
    let sys = ctx.systems.get_or_spawn(SYS_ID);
    let mut cs = ctx.build_type_combat_state(626);
    if cs.max_shield_hp <= 0.0 {
        cs.max_shield_hp = 5_000.0;
        cs.shield_hp = 5_000.0;
        cs.max_armor_hp = 5_000.0;
        cs.armor_hp = 5_000.0;
        cs.max_structure_hp = 5_000.0;
        cs.structure_hp = 5_000.0;
    }
    cs.capacitor = 5_000.0;
    cs.max_capacitor = 5_000.0;
    let _ = sys
        .spawn_ball_with_combat(
            Ball {
                id: ship_id,
                type_id: 626,
                position: pos,
                velocity: DVec3::ZERO,
                max_velocity: 400.0,
                inertia: 1.0,
                mass: 10_000_000.0,
                radius: 150.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            cs,
        )
        .await;
}

#[tokio::test]
async fn test_fighter_heavy_long_range_bomb() {
    // parity: feat:drones.fighter-heavy-long-range-bomb
    eve_testkit::attest(&["feat:drones.fighter-heavy-long-range-bomb"]);

    let ctx = crate::modules_advanced_tactical::build_ctx();
    let kw = HashMap::new();
    let char_id = 90_000_001;
    let carrier_id = 104_000_001;
    let victim_near = 104_000_010;
    let victim_far = 104_000_011;

    setup_carrier(&ctx, char_id, carrier_id).await;
    setup_target_cruiser(&ctx, 90_000_002, victim_near, DVec3::new(26_000.0, 0.0, 0.0)).await;
    setup_target_cruiser(&ctx, 90_000_002, victim_far, DVec3::new(55_000.0, 0.0, 0.0)).await;
    let mut sess = make_sess(char_id, carrier_id);

    let item_id = 410_001;
    crate::open_feats_b35::give_item(&ctx, item_id, AMETAT_HEAVY_FIGHTER, char_id, carrier_id, InventoryFlag::FighterBay, 3);
    dispatch_combat_rpc(&ctx, &mut sess, "fighterMgr", "LoadFightersToTube", &[PyRep::Integer(0), PyRep::Integer(item_id)], &kw)
        .await
        .expect("LoadFightersToTube");
    let launched = dispatch_combat_rpc(
        &ctx,
        &mut sess,
        "fighterMgr",
        "LaunchFightersFromTubes",
        &[PyRep::List(vec![PyRep::Integer(0)])],
        &kw,
    )
    .await
    .expect("LaunchFightersFromTubes");
    let PyRep::List(balls) = launched else { panic!("Expected List"); };
    let fighter_ball = balls[0].as_i64().unwrap();

    let sys = ctx.systems.get_or_spawn(SYS_ID);
    let near_before = sys.get_combat_state(victim_near).await.unwrap().unwrap().shield_hp;
    let far_before = sys.get_combat_state(victim_far).await.unwrap().unwrap().shield_hp;

    let pt = PyRep::Tuple(vec![PyRep::Real(25_000.0), PyRep::Real(0.0), PyRep::Real(0.0)]);
    let res = dispatch_combat_rpc(
        &ctx,
        &mut sess,
        "fighterMgr",
        "CmdActivateAbilitySlots",
        &[PyRep::Integer(fighter_ball), PyRep::Integer(2), pt],
        &kw,
    )
    .await
    .expect("Activate Heavy Fighter Bomb");
    assert_eq!(res.dict_get("success"), Some(&PyRep::Bool(true)));

    let near_after = sys.get_combat_state(victim_near).await.unwrap().unwrap().shield_hp;
    let far_after = sys.get_combat_state(victim_far).await.unwrap().unwrap().shield_hp;
    assert!(near_after < near_before, "Ship within bomb blast radius must take AoE damage");
    assert_eq!(far_after, far_before, "Ship 30km away must be outside bomb blast radius");

    let charges = res.dict_get("chargesRemaining").and_then(|v| v.as_i64()).unwrap_or(0);
    assert!(charges < 3, "Launching Bomb must consume charge ({charges} < 3)");
}

#[tokio::test]
async fn test_fighter_refuel_rearm_repair() {
    // parity: feat:drones.fighter-refuel-rearm-repair
    eve_testkit::attest(&["feat:drones.fighter-refuel-rearm-repair"]);

    let ctx = crate::modules_advanced_tactical::build_ctx();
    let kw = HashMap::new();
    let char_id = 90_000_001;
    let carrier_id = 104_000_001;
    setup_carrier(&ctx, char_id, carrier_id).await;
    let mut sess = make_sess(char_id, carrier_id);

    let tube_item = 410_010;
    let bay_spare = 410_011;
    crate::open_feats_b35::give_item(&ctx, tube_item, EQUITE_LIGHT_FIGHTER, char_id, carrier_id, InventoryFlag::FighterBay, 9);
    crate::open_feats_b35::give_item(&ctx, bay_spare, EQUITE_LIGHT_FIGHTER, char_id, carrier_id, InventoryFlag::FighterBay, 6);

    dispatch_combat_rpc(&ctx, &mut sess, "fighterMgr", "LoadFightersToTube", &[PyRep::Integer(0), PyRep::Integer(tube_item)], &kw)
        .await
        .expect("LoadFightersToTube");
    let launched = dispatch_combat_rpc(
        &ctx,
        &mut sess,
        "fighterMgr",
        "LaunchFightersFromTubes",
        &[PyRep::List(vec![PyRep::Integer(0)])],
        &kw,
    )
    .await
    .expect("LaunchFightersFromTubes");
    let PyRep::List(balls) = launched else { panic!("Expected List"); };
    let fighter_ball = balls[0].as_i64().unwrap();

    // Destroy 2 fighter units via damage
    let (lost, remaining, wiped) = apply_damage_to_fighter_squadron(&ctx, SYS_ID, fighter_ball, DamageVector::new(2_500.0, 0.0, 0.0, 0.0)).await;
    assert!(!wiped && lost >= 1 && remaining < 9, "Damaged squadron must lose fighter units ({lost} lost, {remaining} remaining)");

    // Recall to tube -> triggers rearm/repair/replenish timer
    dispatch_combat_rpc(
        &ctx,
        &mut sess,
        "fighterMgr",
        "RecallFightersToTubes",
        &[PyRep::List(vec![PyRep::Integer(fighter_ball)])],
        &kw,
    )
    .await
    .expect("RecallFightersToTubes");

    let is_rearming = with_combat_runtime(&ctx, |rt| {
        rt.fighter_tube_rearm_until
            .get(&(carrier_id, 0))
            .map(|&exp| exp > ctx.now_unix())
            .unwrap_or(false)
    });
    assert!(is_rearming, "Recalled squadron must enter tube rearm/repair timer");
    assert!(
        dispatch_combat_rpc(&ctx, &mut sess, "fighterMgr", "LaunchFightersFromTubes", &[PyRep::List(vec![PyRep::Integer(0)])], &kw)
            .await
            .is_err(),
        "Relaunching tube while rearm/repair timer is active must be rejected"
    );

    // Advance past rearm timer -> relaunch succeeds!
    ctx.advance_time_seconds(60);
    assert!(
        dispatch_combat_rpc(&ctx, &mut sess, "fighterMgr", "LaunchFightersFromTubes", &[PyRep::List(vec![PyRep::Integer(0)])], &kw)
            .await
            .is_ok(),
        "Relaunch after rearm/repair completion must succeed"
    );
}

#[tokio::test]
async fn test_fighter_support_abilities_and_squadron_scaling() {
    // parity: feat:drones.fighter-support-cenotaph-scarab
    // parity: feat:drones.support-fighter-abilities
    eve_testkit::attest(&[
        "feat:drones.fighter-support-cenotaph-scarab",
        "feat:drones.support-fighter-abilities",
    ]);

    let ctx = crate::modules_advanced_tactical::build_ctx();
    let kw = HashMap::new();
    let char_id = 90_000_001;
    let carrier_id = 104_000_001;
    let target_id = 104_000_020;

    setup_carrier(&ctx, char_id, carrier_id).await;
    setup_target_cruiser(&ctx, 90_000_002, target_id, DVec3::new(5_000.0, 0.0, 0.0)).await;
    let mut sess = make_sess(char_id, carrier_id);

    // Archon has class limit 2 for Support Fighters: test Siren + Cenobite, then recall & test Dromi + Scarab!
    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "AddTarget", &[PyRep::Integer(target_id)], &kw, None)
        .await
        .expect("AddTarget");
    let sys_t = ctx.systems.get_or_spawn(SYS_ID);
    for _ in 0..25 {
        ctx.advance_time_seconds(1);
        let _ = sys_t.try_step_full_tick(1.0);
    }

    for &(pair, base_id) in &[
        ([SIREN_SUPPORT_FIGHTER, CENOBITE_SUPPORT_FIGHTER], 410_020i64),
        ([DROMI_SUPPORT_FIGHTER, SCARAB_SUPPORT_FIGHTER], 410_030i64),
    ] {
        for (idx, tid) in pair.into_iter().enumerate() {
            let iid = base_id + idx as i64;
            crate::open_feats_b35::give_item(&ctx, iid, tid, char_id, carrier_id, InventoryFlag::FighterBay, 3);
            dispatch_combat_rpc(&ctx, &mut sess, "fighterMgr", "LoadFightersToTube", &[PyRep::Integer(idx as i64), PyRep::Integer(iid)], &kw)
                .await
                .expect("Load support fighter");
        }
        let launched = dispatch_combat_rpc(
            &ctx,
            &mut sess,
            "fighterMgr",
            "LaunchFightersFromTubes",
            &[PyRep::List(vec![PyRep::Integer(0), PyRep::Integer(1)])],
            &kw,
        )
        .await
        .expect("Launch support squadrons");
        let PyRep::List(balls) = launched else { panic!("Expected List"); };
        assert_eq!(balls.len(), 2);

        let sys = ctx.systems.get_or_spawn(SYS_ID);

        for (idx, b) in balls.iter().enumerate() {
            let bid = b.as_i64().unwrap();
            let slot = if pair[idx] == CENOBITE_SUPPORT_FIGHTER { 0 } else { 2 };
            let res = dispatch_combat_rpc(
                &ctx,
                &mut sess,
                "fighterMgr",
                "CmdActivateAbilitySlots",
                &[PyRep::Integer(bid), PyRep::Integer(slot), PyRep::Integer(target_id)],
                &kw,
            )
            .await
            .expect("Activate support fighter EWAR ability");
            assert_eq!(res.dict_get("success"), Some(&PyRep::Bool(true)));
        }

        if pair[0] == SIREN_SUPPORT_FIGHTER {
            let scramble = sys.get_combat_state(target_id).await.unwrap().unwrap().warp_scramble_strength;
            assert!(scramble >= 3, "Siren support fighter squadron (3 units) must apply >= 3 warp disruption points (got {scramble})");
            let siren_ball = balls[0].as_i64().unwrap();
            let (lost, rem, _) = apply_damage_to_fighter_squadron(&ctx, SYS_ID, siren_ball, DamageVector::new(4_500.0, 0.0, 0.0, 0.0)).await;
            assert!(lost >= 1 && rem < 3, "Siren squadron must lose unit(s) under fire ({lost} lost, {rem} remaining)");
        }
        dispatch_combat_rpc(
            &ctx,
            &mut sess,
            "fighterMgr",
            "RecallFightersToTubes",
            &[PyRep::List(balls)],
            &kw,
        )
        .await
        .expect("Recall support squadrons");
        ctx.advance_time_seconds(60);
    }
}

#[tokio::test]
async fn test_light_fighter_space_superiority() {
    // parity: feat:drones.light-fighter-space-superiority
    eve_testkit::attest(&["feat:drones.light-fighter-space-superiority"]);

    let ctx = crate::modules_advanced_tactical::build_ctx();
    let kw = HashMap::new();
    let char_id = 90_000_001;
    let carrier_id = 104_000_001;
    let enemy_carrier_id = 104_000_099;

    setup_carrier(&ctx, char_id, carrier_id).await;
    setup_target_cruiser(&ctx, 90_000_002, enemy_carrier_id, DVec3::new(15_000.0, 0.0, 0.0)).await;
    let mut sess = make_sess(char_id, carrier_id);

    let equite_id = 410_050;
    crate::open_feats_b35::give_item(&ctx, equite_id, EQUITE_LIGHT_FIGHTER, char_id, carrier_id, InventoryFlag::FighterBay, 9);
    dispatch_combat_rpc(&ctx, &mut sess, "fighterMgr", "LoadFightersToTube", &[PyRep::Integer(0), PyRep::Integer(equite_id)], &kw)
        .await
        .expect("Load Equite");
    let launched = dispatch_combat_rpc(
        &ctx,
        &mut sess,
        "fighterMgr",
        "LaunchFightersFromTubes",
        &[PyRep::List(vec![PyRep::Integer(0)])],
        &kw,
    )
    .await
    .expect("Launch Equite");
    let PyRep::List(balls) = launched else { panic!("Expected List"); };
    let equite_ball = balls[0].as_i64().unwrap();

    ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "AddTarget", &[PyRep::Integer(enemy_carrier_id)], &kw, None)
        .await
        .expect("AddTarget");
    let sys_t = ctx.systems.get_or_spawn(SYS_ID);
    for _ in 0..25 {
        ctx.advance_time_seconds(1);
        let _ = sys_t.try_step_full_tick(1.0);
    }

    // Activate Evasive Maneuvers / MWD (slot 1) and Anti-Fighter Missile Volley (slot 2)
    let evasive = dispatch_combat_rpc(
        &ctx,
        &mut sess,
        "fighterMgr",
        "CmdActivateAbilitySlots",
        &[PyRep::Integer(equite_ball), PyRep::Integer(1), PyRep::None],
        &kw,
    )
    .await
    .expect("Evasive Maneuvers");
    assert_eq!(evasive.dict_get("success"), Some(&PyRep::Bool(true)));

    let volley = dispatch_combat_rpc(
        &ctx,
        &mut sess,
        "fighterMgr",
        "CmdActivateAbilitySlots",
        &[PyRep::Integer(equite_ball), PyRep::Integer(2), PyRep::Integer(enemy_carrier_id)],
        &kw,
    )
    .await
    .expect("Anti-Fighter Missile Volley");
    assert_eq!(volley.dict_get("success"), Some(&PyRep::Bool(true)));

    // Immediate re-activation of Anti-Fighter Missile Volley must be rejected while on cooldown!
    assert!(
        dispatch_combat_rpc(
            &ctx,
            &mut sess,
            "fighterMgr",
            "CmdActivateAbilitySlots",
            &[PyRep::Integer(equite_ball), PyRep::Integer(2), PyRep::Integer(enemy_carrier_id)],
            &kw,
        )
        .await
        .is_err(),
        "Re-activating Equite anti-fighter missile volley while on cooldown must be rejected"
    );
}
