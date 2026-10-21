//! Integration tests for `S-2` (Carrier Fighter Squadrons & `fighterMgr`)
//! and `S-3` (Pharolux Cyno Beacon & `structureCynoBeaconMgr`).

use crate::common;

use eve_db::types::{InventoryFlag, InventoryItemRecord, UpwellStructureRecord};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use glam::DVec3;
use std::collections::HashMap;

#[tokio::test]
async fn test_s2_fighter_manager_load_launch_recall_movement_abilities_abandon() {
    // parity: plan:S-2
    // parity: feat:drones.fighter-load-tubes, feat:drones.fighter-launch-recall, feat:drones.fighter-movement, feat:drones.fighter-abilities, feat:drones.fighter-abandon
    eve_testkit::attest(&[
        "plan:S-2",
        "feat:drones.fighter-load-tubes",
        "feat:drones.fighter-launch-recall",
        "feat:drones.fighter-movement",
        "feat:drones.fighter-abilities",
        "feat:drones.fighter-abandon",
    ]);

    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let mut sess = SessionState::new(2004);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(90_000_001)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID");

    let ship_id = 100_000_001;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ship) = snap.items.get_mut(&ship_id) {
            ship.type_id = 23911; // Thanatos (Carrier with 4 fighter tubes)
        }
    });

    let fighter1_item_id = 70_000_001;
    let fighter2_item_id = 70_000_002;
    let fighter3_item_id = 70_000_003;
    ctx.db.with_mut_snapshot(|snap| {
        for (fid, ftype, qty) in [
            (fighter1_item_id, 23057, 15), // Dragonfly I (Light Fighter, max size 6)
            (fighter2_item_id, 23059, 6),  // Firbolg I (Light Fighter, max size 6)
            (fighter3_item_id, 40362, 3),  // Ametat I (Heavy Fighter with ability 7)
        ] {
            snap.items.insert(
                fid,
                InventoryItemRecord {
                    item_id: fid,
                    type_id: ftype,
                    owner_id: 90_000_001,
                    location_id: ship_id,
                    flag: InventoryFlag::FighterBay,
                    quantity: qty,
                    singleton: true,
                    version: 1,
                    dynamic_attributes: HashMap::new(),
                    source_mutaplasmid_id: None,
                    insured_payout_isk: 0.0,
                    is_blueprint_copy: false,
                    blueprint_runs_remaining: None,
                },
            );
        }
    });

    // 1. GetFightersForShip: verify fighters in bay
    let list_res = ctx
        .dispatch_rpc(
            &mut sess,
            "fighterMgr",
            None,
            "GetFightersForShip",
            &[PyRep::Integer(ship_id)],
            &kw,
            None,
        )
        .await
        .expect("GetFightersForShip");
    let PyRep::Rowset { columns, .. } = list_res else {
        panic!("Expected rowset from GetFightersForShip");
    };
    assert_eq!(columns, vec!["itemID", "typeID", "squadronSize"]);

    // 2. Load fighter tubes (feat:drones.fighter-load-tubes)
    let err_invalid_tube = ctx
        .dispatch_rpc(
            &mut sess,
            "fighterMgr",
            None,
            "LoadFightersToTube",
            &[PyRep::Integer(99), PyRep::Integer(fighter1_item_id)],
            &kw,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(err_invalid_tube.code, "InvalidFighterTube");

    let load_res1 = ctx
        .dispatch_rpc(
            &mut sess,
            "fighterMgr",
            None,
            "LoadFightersToTube",
            &[PyRep::Integer(0), PyRep::Integer(fighter1_item_id)],
            &kw,
            None,
        )
        .await
        .expect("Load tube 0");
    let PyRep::KeyVal(kv1) = load_res1 else {
        panic!("Expected KeyVal from LoadFightersToTube");
    };
    assert_eq!(
        kv1.get("fighterID").unwrap().as_i64().unwrap(),
        fighter1_item_id
    );
    assert_eq!(
        ctx.db.with_snapshot(|snap| snap.items[&fighter1_item_id].quantity),
        6,
        "Must clamp loaded item quantity to max squadron size 6"
    );
    let remaining_in_bay: i32 = ctx.db.with_snapshot(|snap| {
        snap.items
            .values()
            .filter(|i| {
                i.location_id == ship_id
                    && i.flag == InventoryFlag::FighterBay
                    && i.type_id == 23057
                    && i.item_id != fighter1_item_id
            })
            .map(|i| i.quantity)
            .sum()
    });
    assert_eq!(
        remaining_in_bay, 9,
        "Excess fighters must remain in bay as split stack"
    );

    let err_already_loaded = ctx
        .dispatch_rpc(
            &mut sess,
            "fighterMgr",
            None,
            "LoadFightersToTube",
            &[PyRep::Integer(1), PyRep::Integer(fighter1_item_id)],
            &kw,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(err_already_loaded.code, "FighterAlreadyLoaded");

    let load_res2 = ctx
        .dispatch_rpc(
            &mut sess,
            "fighterMgr",
            None,
            "LoadFightersToTube",
            &[PyRep::Integer(1), PyRep::Integer(fighter2_item_id)],
            &kw,
            None,
        )
        .await
        .expect("Load tube 1");
    assert!(matches!(load_res2, PyRep::KeyVal(_)));

    let load_res3 = ctx
        .dispatch_rpc(
            &mut sess,
            "fighterMgr",
            None,
            "LoadFightersToTube",
            &[PyRep::Integer(2), PyRep::Integer(fighter3_item_id)],
            &kw,
            None,
        )
        .await
        .expect("Load tube 2");
    assert!(matches!(load_res3, PyRep::KeyVal(_)));

    // 3. Undock and launch fighters into space (feat:drones.fighter-launch-recall)
    ctx.dispatch_rpc(&mut sess, "stationSvc", None, "Undock", &[], &kw, None)
        .await
        .expect("Undock");

    let err_empty_tube = ctx
        .dispatch_rpc(
            &mut sess,
            "fighterMgr",
            None,
            "LaunchFightersFromTubes",
            &[PyRep::List(vec![PyRep::Integer(3)])],
            &kw,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(err_empty_tube.code, "EmptyFighterTube");

    let launch_res = ctx
        .dispatch_rpc(
            &mut sess,
            "fighterMgr",
            None,
            "LaunchFightersFromTubes",
            &[PyRep::List(vec![
                PyRep::Integer(0),
                PyRep::Integer(1),
                PyRep::Integer(2),
            ])],
            &kw,
            None,
        )
        .await
        .expect("Launch fighters");
    let PyRep::List(launched_balls) = launch_res else {
        panic!("Expected List from LaunchFightersFromTubes");
    };
    assert_eq!(launched_balls.len(), 3, "All 3 loaded tubes must launch");
    let f1_ball_id = launched_balls[0].as_i64().unwrap();
    let f2_ball_id = launched_balls[1].as_i64().unwrap();
    let f3_ball_id = launched_balls[2].as_i64().unwrap();

    let sys_id = 30_000_142;
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    assert!(
        sys_handle.get_ball_snapshot(f1_ball_id).await.unwrap().is_some(),
        "Fighter 1 must exist as ball in ballpark"
    );

    let err_already_launched = ctx
        .dispatch_rpc(
            &mut sess,
            "fighterMgr",
            None,
            "LaunchFightersFromTubes",
            &[PyRep::List(vec![PyRep::Integer(0)])],
            &kw,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(err_already_launched.code, "FighterAlreadyLaunched");

    let err_unload_launched = ctx
        .dispatch_rpc(
            &mut sess,
            "fighterMgr",
            None,
            "UnloadTubeToFighterBay",
            &[PyRep::Integer(0)],
            &kw,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(err_unload_launched.code, "FighterAlreadyLaunched");

    // 4. Fighter Movement (feat:drones.fighter-movement)
    let (_, f1_before_move) = sys_handle.get_ball_snapshot(f1_ball_id).await.unwrap().unwrap();
    let move_res = ctx
        .dispatch_rpc(
            &mut sess,
            "fighterMgr",
            None,
            "ExecuteMovementCommandOnFighters",
            &[
                PyRep::List(vec![PyRep::Integer(f1_ball_id)]),
                PyRep::String("GOTO_POINT".into()),
                PyRep::Real(10_000.0),
                PyRep::Real(0.0),
                PyRep::Real(0.0),
            ],
            &kw,
            None,
        )
        .await
        .expect("ExecuteMovementCommandOnFighters GOTO_POINT");
    assert!(matches!(move_res, PyRep::Bool(true)));

    let orbit_res = ctx
        .dispatch_rpc(
            &mut sess,
            "fighterMgr",
            None,
            "ExecuteMovementCommandOnFighters",
            &[
                PyRep::List(vec![PyRep::Integer(f2_ball_id)]),
                PyRep::String("ORBIT".into()),
                PyRep::Integer(ship_id),
                PyRep::Real(2_000.0),
            ],
            &kw,
            None,
        )
        .await
        .expect("ExecuteMovementCommandOnFighters ORBIT");
    assert!(matches!(orbit_res, PyRep::Bool(true)));

    for _ in 0..3 {
        sys_handle.step_manual(1.0).await.expect("step fighter movement");
    }
    let (_, f1_after_goto) = sys_handle.get_ball_snapshot(f1_ball_id).await.unwrap().unwrap();
    assert!(
        matches!(f1_after_goto.mode, MovementMode::GotoDirection { .. }),
        "GOTO_POINT must set fighter ball mode to GotoDirection"
    );
    assert!(
        f1_after_goto.position.x > f1_before_move.position.x + 100.0
            && f1_after_goto.velocity.x > 0.0,
        "GOTO_POINT(+X) must advance fighter ball position along +X across ticks"
    );

    let (_, f2_after_orbit) = sys_handle.get_ball_snapshot(f2_ball_id).await.unwrap().unwrap();
    assert_eq!(
        f2_after_orbit.mode,
        MovementMode::Orbit {
            target_id: ship_id,
            range: 2_000.0,
        },
        "ORBIT must set fighter ball mode to Orbit(ship_id, 2000.0)"
    );
    assert!(
        f2_after_orbit.velocity.length() > 0.0,
        "Orbiting fighter ball must have non-zero orbital velocity after ballpark ticks"
    );

    let (_, ship_ball_now) = sys_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    let dist_f1_before_approach = f1_after_goto.position.distance(ship_ball_now.position);
    let approach_res = ctx
        .dispatch_rpc(
            &mut sess,
            "fighterMgr",
            None,
            "ExecuteMovementCommandOnFighters",
            &[
                PyRep::List(vec![PyRep::Integer(f1_ball_id)]),
                PyRep::String("APPROACH".into()),
                PyRep::Integer(ship_id),
                PyRep::Real(500.0),
            ],
            &kw,
            None,
        )
        .await
        .expect("ExecuteMovementCommandOnFighters APPROACH");
    assert!(matches!(approach_res, PyRep::Bool(true)));
    for _ in 0..3 {
        sys_handle.step_manual(1.0).await.expect("step fighter approach");
    }
    let (_, f1_after_approach) = sys_handle.get_ball_snapshot(f1_ball_id).await.unwrap().unwrap();
    assert_eq!(
        f1_after_approach.mode,
        MovementMode::Follow {
            target_id: ship_id,
            range: 500.0,
        },
        "APPROACH must set fighter ball mode to Follow(ship_id, 500.0)"
    );
    assert!(
        f1_after_approach.position.distance(ship_ball_now.position) < dist_f1_before_approach,
        "APPROACH must move fighter ball closer to target ship across ticks"
    );

    // 5. Fighter Abilities (feat:drones.fighter-abilities)
    let (_, f1_ball) = sys_handle
        .get_ball_snapshot(f1_ball_id)
        .await
        .unwrap()
        .unwrap();
    let dummy_target_id = 99_001;
    let dummy_ball = Ball {
        id: dummy_target_id,
        type_id: 282,
        position: f1_ball.position + DVec3::new(1_000.0, 0.0, 0.0),
        velocity: DVec3::ZERO,
        max_velocity: 0.0,
        inertia: 1.0,
        mass: 1_000.0,
        radius: 10.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    let mut dummy_cs = ctx.build_type_combat_state(282);
    dummy_cs.max_shield_hp = 50_000.0;
    dummy_cs.shield_hp = 50_000.0;
    dummy_cs.max_armor_hp = 50_000.0;
    dummy_cs.armor_hp = 50_000.0;
    dummy_cs.max_structure_hp = 50_000.0;
    dummy_cs.structure_hp = 50_000.0;
    sys_handle
        .spawn_ball_with_combat(dummy_ball, dummy_cs)
        .await
        .expect("spawn dummy");
    sys_handle
        .add_target_lock(
            ship_id,
            eve_destiny::TargetLock {
                target_id: dummy_target_id,
                lock_time_ms: 1_000.0,
                remaining_lock_ms: 0.0,
                locked: true,
                max_range_m: Some(100_000.0),
            },
        )
        .await
        .expect("lock dummy target");

    let mut sess_other = SessionState::new(2005);
    sess_other.user_id = Some(1002);
    sess_other.character_id = Some(90_000_999);
    sess_other.solar_system_id = Some(sys_id);
    sess_other.ship_id = Some(100_000_999);
    let err_not_owned = ctx
        .dispatch_rpc(
            &mut sess_other,
            "fighterMgr",
            None,
            "CmdActivateAbilitySlots",
            &[
                PyRep::List(vec![PyRep::Integer(f1_ball_id)]),
                PyRep::Integer(0),
                PyRep::Integer(dummy_target_id),
            ],
            &kw,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(err_not_owned.code, "FighterNotOwned");

    let far_target_id = 99_002;
    let far_ball = Ball {
        id: far_target_id,
        type_id: 282,
        position: f1_ball.position + DVec3::new(200_000.0, 0.0, 0.0),
        velocity: DVec3::ZERO,
        max_velocity: 0.0,
        inertia: 1.0,
        mass: 1_000.0,
        radius: 10.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    sys_handle
        .spawn_ball_with_combat(far_ball, ctx.build_type_combat_state(282))
        .await
        .expect("spawn far dummy");
    sys_handle
        .add_target_lock(
            ship_id,
            eve_destiny::TargetLock {
                target_id: far_target_id,
                lock_time_ms: 1_000.0,
                remaining_lock_ms: 0.0,
                locked: true,
                max_range_m: Some(300_000.0),
            },
        )
        .await
        .expect("lock far dummy target");
    let err_out_of_range = ctx
        .dispatch_rpc(
            &mut sess,
            "fighterMgr",
            None,
            "CmdActivateAbilitySlots",
            &[
                PyRep::List(vec![PyRep::Integer(f1_ball_id)]),
                PyRep::Integer(0),
                PyRep::Integer(far_target_id),
            ],
            &kw,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(err_out_of_range.code, "TargetOutOfRange");

    let sec_before_npc = ctx.db.with_snapshot(|s| s.characters[&90_000_001].security_status);
    let cs_before = sys_handle.get_combat_state(dummy_target_id).await.unwrap().unwrap();
    let ability_res = ctx
        .dispatch_rpc(
            &mut sess,
            "fighterMgr",
            None,
            "CmdActivateAbilitySlots",
            &[
                PyRep::List(vec![PyRep::Integer(f1_ball_id)]),
                PyRep::Integer(0),
                PyRep::Integer(dummy_target_id),
            ],
            &kw,
            None,
        )
        .await
        .expect("CmdActivateAbilitySlots Slot 0");
    assert!(matches!(ability_res, PyRep::KeyVal(_)));
    let cs_after = sys_handle.get_combat_state(dummy_target_id).await.unwrap().unwrap();
    assert!(
        cs_after.shield_hp < cs_before.shield_hp,
        "Fighter slot 0 attack must deal damage to target"
    );

    let now_npc = ctx.now_unix();
    {
        let cw = ctx.crimewatch.read().unwrap();
        let att_st = cw.states.get(&90_000_001).expect("attacker crimewatch state");
        assert!(att_st.is_weapon_timer_active(now_npc));
        assert!(!att_st.is_criminal(now_npc));
        assert!(!att_st.is_suspect(now_npc));
        assert!(att_st.pending_concord_arrival.is_none());
    }
    let sec_after_npc = ctx.db.with_snapshot(|s| s.characters[&90_000_001].security_status);
    assert_eq!(sec_after_npc, sec_before_npc);

    let err_cooldown = ctx
        .dispatch_rpc(
            &mut sess,
            "fighterMgr",
            None,
            "CmdActivateAbilitySlots",
            &[
                PyRep::List(vec![PyRep::Integer(f1_ball_id)]),
                PyRep::Integer(0),
                PyRep::Integer(dummy_target_id),
            ],
            &kw,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(err_cooldown.code, "AbilityOnCooldown");

    let mwd_res = ctx
        .dispatch_rpc(
            &mut sess,
            "fighterMgr",
            None,
            "CmdActivateAbilitySlots",
            &[PyRep::List(vec![PyRep::Integer(f1_ball_id)]), PyRep::Integer(1)],
            &kw,
            None,
        )
        .await
        .expect("CmdActivateAbilitySlots Slot 1 MWD");
    assert!(matches!(mwd_res, PyRep::KeyVal(_)));
    assert!(ctx.active_fighters.read().unwrap()[&f1_ball_id].mwd_active);

    let cs_before_salvo = sys_handle.get_combat_state(dummy_target_id).await.unwrap().unwrap();
    let salvo_res = ctx
        .dispatch_rpc(
            &mut sess,
            "fighterMgr",
            None,
            "CmdActivateAbilitySlots",
            &[
                PyRep::List(vec![PyRep::Integer(f1_ball_id)]),
                PyRep::Integer(2),
                PyRep::Integer(dummy_target_id),
            ],
            &kw,
            None,
        )
        .await
        .expect("CmdActivateAbilitySlots Slot 2 Micromissile Salvo");
    let PyRep::KeyVal(salvo_kv) = salvo_res else {
        panic!("Expected KeyVal from Slot 2 Micromissile Salvo");
    };
    let salvo_charges = salvo_kv.get("chargeCount").unwrap().as_i64().unwrap();
    let salvo_rem = salvo_kv.get("remainingCharges").unwrap().as_i64().unwrap();
    assert!(salvo_charges > 0 && salvo_rem == salvo_charges - 1);
    let cs_after_salvo = sys_handle.get_combat_state(dummy_target_id).await.unwrap().unwrap();
    assert!(cs_after_salvo.shield_hp < cs_before_salvo.shield_hp);

    for (idx, (sup_type_id, expected_ability_id)) in
        [(40345_u32, 12_i64), (40346_u32, 10_i64), (40347_u32, 2_i64)]
            .into_iter()
            .enumerate()
    {
        let sup_ball_id = 98_500 + idx as i64;
        let sup_item_id = 70_500 + idx as i64;
        sys_handle
            .spawn_ball_with_combat(
                Ball {
                    id: sup_ball_id,
                    type_id: sup_type_id,
                    position: f1_ball.position + DVec3::new(500.0, 0.0, 0.0),
                    velocity: DVec3::ZERO,
                    max_velocity: 2_500.0,
                    inertia: 0.5,
                    mass: 20_000.0,
                    radius: 20.0,
                    speed_fraction: 0.0,
                    mode: MovementMode::Stop,
                },
                ctx.build_type_combat_state(sup_type_id),
            )
            .await
            .expect("spawn support fighter ball");
        ctx.active_fighters.write().unwrap().insert(
            sup_ball_id,
            eve_server::router::ActiveFighterSquadron {
                ball_id: sup_ball_id,
                item_id: sup_item_id,
                type_id: sup_type_id,
                owner_char_id: 90_000_001,
                parent_ship_id: ship_id,
                tube_index: 3,
                squadron_size: 3,
                target_id: None,
                is_abandoned: false,
                mwd_active: false,
            },
        );
        let sup_res = ctx
            .dispatch_rpc(
                &mut sess,
                "fighterMgr",
                None,
                "CmdActivateAbilitySlots",
                &[
                    PyRep::List(vec![PyRep::Integer(sup_ball_id)]),
                    PyRep::Integer(0),
                    PyRep::Integer(dummy_target_id),
                ],
                &kw,
                None,
            )
            .await
            .expect("CmdActivateAbilitySlots support fighter EWAR");
        let PyRep::KeyVal(sup_kv) = sup_res else {
            panic!("Expected KeyVal from support fighter ability");
        };
        assert_eq!(
            sup_kv.get("abilityID").unwrap().as_i64().unwrap(),
            expected_ability_id
        );
        ctx.active_fighters.write().unwrap().remove(&sup_ball_id);
        let _ = sys_handle.remove_ball(sup_ball_id).await;
    }

    let err_highsec = ctx
        .dispatch_rpc(
            &mut sess,
            "fighterMgr",
            None,
            "CmdActivateAbilitySlots",
            &[PyRep::List(vec![PyRep::Integer(f3_ball_id)]), PyRep::Integer(2)],
            &kw,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(err_highsec.code, "AbilityDisallowedInHighSec");

    let lowsec_sys = 30_000_012;
    let lowsec_handle = ctx.systems.get_or_spawn(lowsec_sys);
    let (_, f3_ball) = sys_handle
        .get_ball_snapshot(f3_ball_id)
        .await
        .unwrap()
        .unwrap();
    let _ = lowsec_handle
        .spawn_ball_with_combat(f3_ball.clone(), ctx.build_type_combat_state(40362))
        .await;
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&90_000_001).unwrap().solar_system_id = lowsec_sys;
    });
    sess.solar_system_id = Some(lowsec_sys);
    let err_lowsec = ctx
        .dispatch_rpc(
            &mut sess,
            "fighterMgr",
            None,
            "CmdActivateAbilitySlots",
            &[PyRep::List(vec![PyRep::Integer(f3_ball_id)]), PyRep::Integer(2)],
            &kw,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(err_lowsec.code, "AbilityDisallowedInLowSec");

    let nullsec_sys = 30_002_904;
    let nullsec_handle = ctx.systems.get_or_spawn(nullsec_sys);
    let _ = nullsec_handle
        .spawn_ball_with_combat(f3_ball.clone(), ctx.build_type_combat_state(40362))
        .await;
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&90_000_001).unwrap().solar_system_id = nullsec_sys;
    });
    sess.solar_system_id = Some(nullsec_sys);
    let bomb_res = ctx
        .dispatch_rpc(
            &mut sess,
            "fighterMgr",
            None,
            "CmdActivateAbilitySlots",
            &[PyRep::List(vec![PyRep::Integer(f3_ball_id)]), PyRep::Integer(2)],
            &kw,
            None,
        )
        .await
        .expect("Ametat I Slot 2 Bomb must succeed in null-sec");
    let PyRep::KeyVal(bomb_kv) = bomb_res else {
        panic!("Expected KeyVal from Ametat I Slot 2 Bomb");
    };
    assert_eq!(bomb_kv.get("abilityID").unwrap().as_i64().unwrap(), 7);
    let bomb_charges = bomb_kv.get("chargeCount").unwrap().as_i64().unwrap();
    let bomb_rem = bomb_kv.get("remainingCharges").unwrap().as_i64().unwrap();
    assert!(bomb_charges > 0 && bomb_rem == bomb_charges - 1);

    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&90_000_001).unwrap().solar_system_id = sys_id;
    });
    sess.solar_system_id = Some(sys_id);

    let victim_char_id = 90_000_002;
    let victim_ship_id = 100_000_002;
    ctx.db.with_mut_snapshot(|snap| {
        let mut vic_ch = snap.characters[&90_000_001].clone();
        vic_ch.character_id = victim_char_id;
        vic_ch.name = "Victim Pilot".to_string();
        vic_ch.security_status = 5.0;
        vic_ch.active_ship_id = victim_ship_id;
        snap.characters.insert(victim_char_id, vic_ch);
        snap.items.insert(
            victim_ship_id,
            InventoryItemRecord {
                item_id: victim_ship_id,
                type_id: 587,
                owner_id: victim_char_id,
                location_id: 60003760,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
    });
    let (_, f2_ball) = sys_handle
        .get_ball_snapshot(f2_ball_id)
        .await
        .unwrap()
        .unwrap();
    let victim_ball = Ball {
        id: victim_ship_id,
        type_id: 587,
        position: f2_ball.position + DVec3::new(1_000.0, 0.0, 0.0),
        velocity: DVec3::ZERO,
        max_velocity: 0.0,
        inertia: 1.0,
        mass: 1_000.0,
        radius: 30.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    sys_handle
        .spawn_ball_with_combat(victim_ball, ctx.build_type_combat_state(587))
        .await
        .expect("spawn victim ship");
    sys_handle
        .add_target_lock(
            ship_id,
            eve_destiny::TargetLock {
                target_id: victim_ship_id,
                lock_time_ms: 1_000.0,
                remaining_lock_ms: 0.0,
                locked: true,
                max_range_m: Some(100_000.0),
            },
        )
        .await
        .expect("lock victim ship");

    let sec_before = ctx.db.with_snapshot(|s| s.characters[&90_000_001].security_status);
    let pvp_res = ctx
        .dispatch_rpc(
            &mut sess,
            "fighterMgr",
            None,
            "CmdActivateAbilitySlots",
            &[
                PyRep::List(vec![PyRep::Integer(f2_ball_id)]),
                PyRep::Integer(0),
                PyRep::Integer(victim_ship_id),
            ],
            &kw,
            None,
        )
        .await
        .expect("CmdActivateAbilitySlots Slot 0 PvP attack");
    assert!(matches!(pvp_res, PyRep::KeyVal(_)));

    let now = ctx.now_unix();
    {
        let cw = ctx.crimewatch.read().unwrap();
        let att_st = cw.states.get(&90_000_001).expect("attacker crimewatch state");
        assert!(att_st.is_weapon_timer_active(now));
        assert!(att_st.is_criminal(now));
        assert!(att_st.pending_concord_arrival.is_some());
        assert!(
            cw.aggression_mgr
                .is_aggressor_against(90_000_001, victim_char_id, now)
        );
    }
    let sec_after = ctx.db.with_snapshot(|s| s.characters[&90_000_001].security_status);
    assert!(sec_after < sec_before);

    // 6. Abandon fighters (feat:drones.fighter-abandon)
    let abandon_res = ctx
        .dispatch_rpc(
            &mut sess,
            "fighterMgr",
            None,
            "CmdAbandonFighter",
            &[PyRep::Integer(f1_ball_id)],
            &kw,
            None,
        )
        .await
        .expect("CmdAbandonFighter");
    let PyRep::List(ab_list) = abandon_res else {
        panic!("Expected List from CmdAbandonFighter");
    };
    assert!(!ab_list.is_empty());
    assert!(ctx.active_fighters.read().unwrap()[&f1_ball_id].is_abandoned);

    let err_load_abandoned = ctx
        .dispatch_rpc(
            &mut sess,
            "fighterMgr",
            None,
            "LoadFightersToTube",
            &[PyRep::Integer(0), PyRep::Integer(fighter1_item_id)],
            &kw,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(err_load_abandoned.code, "ItemNotInFighterBay");

    // 7. Recall fighters (feat:drones.fighter-launch-recall)
    let recall_res = ctx
        .dispatch_rpc(
            &mut sess,
            "fighterMgr",
            None,
            "RecallFightersToTubes",
            &[PyRep::List(vec![
                PyRep::Integer(f2_ball_id),
                PyRep::Integer(f3_ball_id),
            ])],
            &kw,
            None,
        )
        .await
        .expect("RecallFightersToTubes");
    let PyRep::List(rec_list) = recall_res else {
        panic!("Expected List from RecallFightersToTubes");
    };
    assert_eq!(rec_list.len(), 2);
    assert!(sys_handle.get_ball_snapshot(f2_ball_id).await.unwrap().is_none());
    assert!(ctx.fighter_tube_loads.read().unwrap().contains_key(&(ship_id, 1)));
    assert!(ctx.fighter_tube_loads.read().unwrap().contains_key(&(ship_id, 2)));
}

#[tokio::test]
async fn test_s3_pharolux_cyno_beacon_jump_bridge_and_group_jump() {
    // parity: plan:S-3
    // parity: feat:navigation.pharolux-beacon
    eve_testkit::attest(&["plan:S-3", "feat:navigation.pharolux-beacon"]);

    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let mut sess = SessionState::new(2005);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(90_000_001)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID");

    let ship_id = 100_000_001;
    let pilot_corp_id = sess.corporation_id.unwrap();
    let dest_sys_id = 30_000_204;
    let beacon_struct_id = 1_040_000_000_200_i64;
    let beacon_pos = DVec3::new(450_000.0, 0.0, 0.0);

    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            beacon_struct_id,
            UpwellStructureRecord {
                structure_id: beacon_struct_id,
                type_id: 35840, // Pharolux Cyno Beacon
                solar_system_id: dest_sys_id,
                owner_corp_id: pilot_corp_id,
                name: "Ohkunen Pharolux Beacon".into(),
                position: beacon_pos.to_array(),
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 50,
                shield_hp: 50_000.0,
                armor_hp: 50_000.0,
                hull_hp: 50_000.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
            },
        );
        snap.items.insert(
            88_000_001,
            InventoryItemRecord {
                item_id: 88_000_001,
                type_id: 16274, // Helium Isotopes
                owner_id: 90_000_001,
                location_id: ship_id,
                flag: InventoryFlag::SpecializedFuelBay,
                quantity: 20_000,
                singleton: false,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
    });

    // Jump drives are SDE-driven: make the hull a real Archon (canJump, Helium Isotopes, 3.5 LY base range)
    // and train Jump Drive Calibration V so Ohkunen (4.80 LY) is in range.
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ship) = snap.items.get_mut(&ship_id) {
            ship.type_id = 23757;
        }
        snap.skills.entry(90_000_001).or_default().insert(
            21611,
            eve_db::TrainedSkillRecord {
                type_id: 21611,
                level: 5,
                skill_points: 256_000.0,
            },
        );
    });
    ctx.invalidate_ship_fitting(ship_id);

    let fleet_beacons = ctx
        .dispatch_rpc(
            &mut sess,
            "structureCynoBeaconMgr",
            None,
            "GetMyAccessibleOnlineCynoBeaconStructures",
            &[],
            &kw,
            None,
        )
        .await
        .expect("GetMyAccessibleOnlineCynoBeaconStructures");
    let PyRep::List(blist) = fleet_beacons else {
        panic!("Expected List from GetMyAccessibleOnlineCynoBeaconStructures");
    };
    assert!(blist.iter().any(|b| {
        matches!(b, PyRep::KeyVal(d) if d.get("structureID").and_then(|v| v.as_i64()) == Some(beacon_struct_id))
    }));

    ctx.dispatch_rpc(&mut sess, "stationSvc", None, "Undock", &[], &kw, None)
        .await
        .expect("Undock");

    let jump_res = ctx
        .dispatch_rpc(
            &mut sess,
            "structureCynoBeaconMgr",
            None,
            "CmdJumpToStructureBeacon",
            &[
                PyRep::Integer(dest_sys_id as i64),
                PyRep::Integer(beacon_struct_id),
            ],
            &kw,
            None,
        )
        .await
        .expect("CmdJumpToStructureBeacon");
    let PyRep::KeyVal(jkv) = jump_res else {
        panic!("Expected KeyVal from CmdJumpToStructureBeacon");
    };
    assert_eq!(
        jkv.get("solarSystemID").unwrap().as_i64().unwrap(),
        dest_sys_id as i64
    );
    assert!(jkv.get("jumpFatigueMinutes").unwrap().as_f64().unwrap() > 0.0);
    assert!(jkv.get("jumpCooldownMinutes").unwrap().as_f64().unwrap() > 0.0);
    assert_eq!(sess.solar_system_id, Some(dest_sys_id));

    let dest_handle = ctx.systems.get_or_spawn(dest_sys_id);
    let (_, dest_ball) = dest_handle
        .get_ball_snapshot(ship_id)
        .await
        .unwrap()
        .expect("Ship spawned in destination system");
    assert!(dest_ball.position.distance(beacon_pos) <= 10_000.0);

    let rem_fuel = ctx.db.with_snapshot(|snap| {
        snap.items.get(&88_000_001).map(|i| i.quantity).unwrap_or(0)
    });
    assert!(rem_fuel < 10_000);

    let bridge_res = ctx
        .dispatch_rpc(
            &mut sess,
            "structureCynoBeaconMgr",
            None,
            "CmdBridgeToStructureBeacon",
            &[
                PyRep::Integer(dest_sys_id as i64),
                PyRep::Integer(beacon_struct_id),
            ],
            &kw,
            None,
        )
        .await
        .expect("CmdBridgeToStructureBeacon");
    let PyRep::KeyVal(b_kv) = bridge_res else {
        panic!("Expected KeyVal from CmdBridgeToStructureBeacon");
    };
    assert_eq!(b_kv.get("bridgeActive").unwrap(), &PyRep::Bool(true));

    let group_res = ctx
        .dispatch_rpc(
            &mut sess,
            "structureCynoBeaconMgr",
            None,
            "CmdGroupJumpToStructureBeacon",
            &[
                PyRep::Integer(dest_sys_id as i64),
                PyRep::Integer(beacon_struct_id),
            ],
            &kw,
            None,
        )
        .await
        .expect("CmdGroupJumpToStructureBeacon");
    let PyRep::KeyVal(g_kv) = group_res else {
        panic!("Expected KeyVal from CmdGroupJumpToStructureBeacon");
    };
    assert_eq!(g_kv.get("groupJumpActive").unwrap(), &PyRep::Bool(true));

    let highsec_beacon_id = 1_040_000_000_201_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            highsec_beacon_id,
            UpwellStructureRecord {
                structure_id: highsec_beacon_id,
                type_id: 35840,
                solar_system_id: 30000142,
                owner_corp_id: pilot_corp_id,
                name: "Jita HighSec Beacon".into(),
                position: [0.0, 0.0, 0.0],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 50,
                shield_hp: 50_000.0,
                armor_hp: 50_000.0,
                hull_hp: 50_000.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
            },
        );
    });

    let hs_err = ctx
        .dispatch_rpc(
            &mut sess,
            "structureCynoBeaconMgr",
            None,
            "CmdJumpToStructureBeacon",
            &[
                PyRep::Integer(30000142),
                PyRep::Integer(highsec_beacon_id),
            ],
            &kw,
            None,
        )
        .await;
    // The capital's own SDE `disallowInHighSec` refusal fires before the beacon-side high-sec rule.
    assert!(matches!(
        hs_err.unwrap_err().code.as_str(),
        "CannotJumpToHighSecBeacon" | "JumpDriveHighSecForbidden"
    ));
}
