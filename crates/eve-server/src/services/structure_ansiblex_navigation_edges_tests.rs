//! Edge-case, ACL denial, fuel/range/mass guard, and multi-stack depletion tests for `structure_ansiblex_navigation`.

#![forbid(unsafe_code)]

use super::*;
use crate::services::fleet_social::fleet_social_fleet_ops::fleet_social_fleet_ops_tests::{
    clone_char_with, test_ctx_with_clock,
};
use crate::services::station_structure::structure_upwell_ops::{load_ext_state, save_ext_state};
use eve_db::{InventoryItemRecord, UpwellStructureRecord};
use eve_destiny::{Ball, CombatState, MovementMode};
use glam::DVec3;

fn make_ansiblex(structure_id: i64, owner_corp_id: i64, solar_system_id: u32) -> UpwellStructureRecord {
    UpwellStructureRecord {
        structure_id,
        type_id: ANSIBLEX_TYPE_ID,
        owner_corp_id,
        solar_system_id,
        name: format!("Ansiblex {structure_id}"),
        position: [10_000.0, 0.0, 0.0],
        state: "ShieldVulnerable".into(),
        quantum_core_installed: true,
        fuel_blocks: 500,
        shield_hp: 1.0,
        armor_hp: 1.0,
        hull_hp: 1.0,
        reinforced_until_unix: 0,
        last_damage_tick: 0,
        last_damage_unix: 0,
        self_destruct_at_unix: None,
    }
}

fn find_two_sov_null_pairs(ctx: &ServerContext) -> ((u32, u32), (u32, u32)) {
    let sov: Vec<u32> = ctx
        .sde
        .solar_systems
        .keys()
        .copied()
        .filter(|&sid| is_sovereign_nullsec_for_ansiblex(&ctx.sde, sid))
        .take(120)
        .collect();
    let close = sov
        .iter()
        .find_map(|&a| {
            sov.iter().find_map(|&b| {
                let d = crate::services::space_destiny::space_nav_travel::distance_ly(ctx, a, b);
                (a != b && d > 0.0 && d <= 4.5).then_some((a, b))
            })
        })
        .expect("close sov pair");
    let far = sov
        .iter()
        .find_map(|&a| {
            sov.iter().find_map(|&b| {
                let d = crate::services::space_destiny::space_nav_travel::distance_ly(ctx, a, b);
                (a != b && d > ANSIBLEX_MAX_RANGE_LY + 1.0).then_some((a, b))
            })
        })
        .expect("far sov pair");
    (close, far)
}

#[tokio::test]
async fn ansiblex_recon_acl_errors_and_destroyed_or_unlinked_filters() {
    let ctx = test_ctx_with_clock(1_800_100_000);
    let ((sys_a, sys_b), _) = find_two_sov_null_pairs(&ctx);

    let owner_corp = 98_000_001_i64;
    let outsider_corp = 98_000_099_i64;
    let char_id = 910_501_i64;
    let ship_id = 910_502_i64;
    let jb_a = 1_000_000_950_001_i64;
    let jb_b = 1_000_000_950_002_i64;
    let jb_destroyed = 1_000_000_950_003_i64;
    let jb_unlinked = 1_000_000_950_004_i64;

    ctx.db.with_mut_snapshot(|snap| {
        let ch = clone_char_with(snap, char_id, "Outsider", outsider_corp, sys_a, ship_id);
        snap.characters.insert(char_id, ch);
        snap.structures.insert(jb_a, make_ansiblex(jb_a, owner_corp, sys_a));
        snap.structures.insert(jb_b, make_ansiblex(jb_b, owner_corp, sys_b));
        let mut dest = make_ansiblex(jb_destroyed, owner_corp, sys_b);
        dest.state = "Destroyed".into();
        snap.structures.insert(jb_destroyed, dest);
        snap.structures.insert(jb_unlinked, make_ansiblex(jb_unlinked, owner_corp, sys_a));

        let mut ext = load_ext_state(snap);
        ext.jump_bridge_links.insert(jb_a, jb_b);
        ext.jump_bridge_links.insert(jb_unlinked, jb_destroyed);
        save_ext_state(snap, &ext);
    });

    // Unauthenticated session checks
    let mut unauth = SessionState::new(1);
    assert!(handle_get_linked_structure(&ctx, &mut unauth, &[PyRep::Integer(jb_a)]).is_err());
    assert!(handle_get_jb_structure_destination(&ctx, &mut unauth, &[PyRep::Integer(jb_a)]).is_err());
    assert!(handle_get_jump_bridges_with_my_access(&ctx, &mut unauth).is_err());
    assert!(handle_get_nearby_jump_bridges(&ctx, &mut unauth).is_err());

    let mut sess = SessionState::new(1);
    sess.user_id = Some(char_id as u64);
    sess.character_id = Some(char_id);
    sess.corporation_id = Some(outsider_corp);
    sess.solar_system_id = Some(sys_a);
    sess.ship_id = Some(ship_id);

    // Missing structureID arg
    assert_eq!(
        handle_get_linked_structure(&ctx, &mut sess, &[]).unwrap_err().code,
        "MissingStructureId"
    );
    assert_eq!(
        handle_get_jb_structure_destination(&ctx, &mut sess, &[]).unwrap_err().code,
        "MissingStructureId"
    );

    // Outsider denied recon access -> AccessDenied
    assert_eq!(
        handle_get_linked_structure(&ctx, &mut sess, &[PyRep::Integer(jb_a)])
            .unwrap_err()
            .code,
        "AccessDenied"
    );
    assert_eq!(
        handle_get_jb_structure_destination(&ctx, &mut sess, &[PyRep::Integer(jb_a)])
            .unwrap_err()
            .code,
        "AccessDenied"
    );

    // Outsider sees no bridges in GetJumpBridgesWithMyAccess / GetNearbyJumpBridges
    let PyRep::Tuple(tup_out) = handle_get_jump_bridges_with_my_access(&ctx, &mut sess).unwrap() else {
        panic!();
    };
    assert_eq!(tup_out[0], PyRep::List(vec![]));
    assert_eq!(tup_out[1], PyRep::List(vec![]));
    assert_eq!(
        handle_get_nearby_jump_bridges(&ctx, &mut sess).unwrap(),
        PyRep::List(vec![])
    );

    // Move char into owner_corp: jb_unlinked links to Destroyed jb_destroyed -> resolve_linked_structure_id returns None
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().corporation_id = owner_corp;
    });
    assert_eq!(
        handle_get_linked_structure(&ctx, &mut sess, &[PyRep::Integer(jb_unlinked)]).unwrap(),
        PyRep::None
    );
    assert_eq!(
        handle_get_jb_structure_destination(&ctx, &mut sess, &[PyRep::Integer(jb_unlinked)])
            .unwrap_err()
            .code,
        "NoLinkedStructure"
    );
    // Destroyed bridge is excluded, unlinked bridge is in accessible_ids but not pairs/nearby
    let PyRep::Tuple(tup_own) = handle_get_jump_bridges_with_my_access(&ctx, &mut sess).unwrap() else {
        panic!();
    };
    let PyRep::List(pairs) = &tup_own[0] else { panic!() };
    let PyRep::List(accessible) = &tup_own[1] else { panic!() };
    assert_eq!(pairs.len(), 1);
    assert_eq!(accessible.len(), 3);
}

#[tokio::test]
async fn ansiblex_jump_all_guards_and_specialized_fuel_bay_multi_stack_depletion() {
    let ctx = test_ctx_with_clock(1_800_200_000);
    let ((sys_a, sys_b), (far_a, far_b)) = find_two_sov_null_pairs(&ctx);

    let corp_id = 98_000_001_i64;
    let char_id = 910_601_i64;
    let ship_id = 910_602_i64;
    let jb_a = 1_000_000_960_001_i64;
    let jb_b = 1_000_000_960_002_i64;
    let jb_far_a = 1_000_000_960_003_i64;
    let jb_far_b = 1_000_000_960_004_i64;

    let titan_tid = ctx
        .sde
        .types
        .values()
        .find(|t| t.group_id == 30)
        .map(|t| t.type_id)
        .unwrap_or(671);

    ctx.db.with_mut_snapshot(|snap| {
        let ch = clone_char_with(snap, char_id, "Gate Pilot", corp_id, sys_a, ship_id);
        snap.characters.insert(char_id, ch);
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 587,
                owner_id: char_id,
                location_id: sys_a as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.structures.insert(jb_a, make_ansiblex(jb_a, corp_id, sys_a));
        snap.structures.insert(jb_b, make_ansiblex(jb_b, corp_id, sys_b));
        snap.structures.insert(jb_far_a, make_ansiblex(jb_far_a, corp_id, far_a));
        snap.structures.insert(jb_far_b, make_ansiblex(jb_far_b, corp_id, far_b));

        let mut ext = load_ext_state(snap);
        ext.jump_bridge_links.insert(jb_a, jb_b);
        ext.jump_bridge_links.insert(jb_far_a, jb_far_b);
        save_ext_state(snap, &ext);
    });

    let mut sess = SessionState::new(1);
    sess.user_id = Some(char_id as u64);
    sess.character_id = Some(char_id);
    sess.corporation_id = Some(corp_id);
    sess.solar_system_id = Some(sys_a);
    sess.ship_id = Some(ship_id);

    // Docked in station
    let mut s_sta = sess.clone();
    s_sta.station_id = Some(60_003_760);
    assert_eq!(
        handle_cmd_jump_through_structure_stargate(&ctx, &mut s_sta, &[PyRep::Integer(jb_a)])
            .await
            .unwrap_err()
            .code,
        "ShipDocked"
    );

    // Missing structureID
    assert_eq!(
        handle_cmd_jump_through_structure_stargate(&ctx, &mut sess, &[])
            .await
            .unwrap_err()
            .code,
        "MissingStructureId"
    );

    // NoSolarSystem / NoActiveShip
    let mut s_nosys = sess.clone();
    s_nosys.solar_system_id = None;
    assert_eq!(
        handle_cmd_jump_through_structure_stargate(&ctx, &mut s_nosys, &[PyRep::Integer(jb_a)])
            .await
            .unwrap_err()
            .code,
        "NoSolarSystem"
    );
    let mut s_noship = sess.clone();
    s_noship.ship_id = None;
    assert_eq!(
        handle_cmd_jump_through_structure_stargate(&ctx, &mut s_noship, &[PyRep::Integer(jb_a)])
            .await
            .unwrap_err()
            .code,
        "NoActiveShip"
    );

    // Pre-spawn dummy balls so pre-ball validation checks reach their branches before ShipNotInSpace
    let sys_a_pre = ctx.systems.get_or_spawn(sys_a);
    let _ = sys_a_pre
        .spawn_ball_with_combat(
            Ball {
                id: ship_id,
                type_id: 0,
                position: DVec3::new(10_500.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 1.0,
                mass: 0.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            CombatState::default(),
        )
        .await;
    let far_a_pre = ctx.systems.get_or_spawn(far_a);
    let _ = far_a_pre
        .spawn_ball_with_combat(
            Ball {
                id: ship_id,
                type_id: 0,
                position: DVec3::new(10_500.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 1.0,
                mass: 0.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            CombatState::default(),
        )
        .await;

    // WeaponsTimerActive
    {
        let mut cw = ctx.crimewatch.write().unwrap();
        cw.get_or_create(char_id).weapon_timer_until_unix = ctx.now_unix() + 60;
    }
    assert_eq!(
        handle_cmd_jump_through_structure_stargate(&ctx, &mut sess, &[PyRep::Integer(jb_a)])
            .await
            .unwrap_err()
            .code,
        "WeaponsTimerActive"
    );
    ctx.advance_secs(120);

    // StructureNotFound
    assert_eq!(
        handle_cmd_jump_through_structure_stargate(&ctx, &mut sess, &[PyRep::Integer(999_999_888)])
            .await
            .unwrap_err()
            .code,
        "StructureNotFound"
    );

    // Offline / unpowered / wrong system / non-Ansiblex source structure
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.get_mut(&jb_a).unwrap().fuel_blocks = 0;
    });
    assert_eq!(
        handle_cmd_jump_through_structure_stargate(&ctx, &mut sess, &[PyRep::Integer(jb_a)])
            .await
            .unwrap_err()
            .code,
        "StructureNotOperational"
    );
    ctx.db.with_mut_snapshot(|snap| {
        let s = snap.structures.get_mut(&jb_a).unwrap();
        s.fuel_blocks = 500;
        s.owner_corp_id = 98_000_099; // denies char_id
    });
    assert_eq!(
        handle_cmd_jump_through_structure_stargate(&ctx, &mut sess, &[PyRep::Integer(jb_a)])
            .await
            .unwrap_err()
            .code,
        "AccessDenied"
    );
    ctx.db.with_mut_snapshot(|snap| {
        let s = snap.structures.get_mut(&jb_a).unwrap();
        s.owner_corp_id = corp_id;
        s.type_id = 35832; // Astrahus, not Ansiblex
    });
    assert_eq!(
        handle_cmd_jump_through_structure_stargate(&ctx, &mut sess, &[PyRep::Integer(jb_a)])
            .await
            .unwrap_err()
            .code,
        "StructureNotOperational"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.get_mut(&jb_a).unwrap().type_id = ANSIBLEX_TYPE_ID;
        snap.structures.get_mut(&jb_b).unwrap().fuel_blocks = 0;
    });
    assert_eq!(
        handle_cmd_jump_through_structure_stargate(&ctx, &mut sess, &[PyRep::Integer(jb_a)])
            .await
            .unwrap_err()
            .code,
        "StructureNotOperational"
    );

    // NoLinkedStructure when link removed
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.get_mut(&jb_b).unwrap().fuel_blocks = 500;
        let mut ext = load_ext_state(snap);
        ext.jump_bridge_links.remove(&jb_a);
        save_ext_state(snap, &ext);
    });
    assert_eq!(
        handle_cmd_jump_through_structure_stargate(&ctx, &mut sess, &[PyRep::Integer(jb_a)])
            .await
            .unwrap_err()
            .code,
        "NoLinkedStructure"
    );

    // JumpBridgeRequiresSovNull (move jb_b to Jita highsec)
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.get_mut(&jb_b).unwrap().solar_system_id = 30_000_142;
        let mut ext = load_ext_state(snap);
        ext.jump_bridge_links.insert(jb_a, jb_b);
        save_ext_state(snap, &ext);
    });
    assert_eq!(
        handle_cmd_jump_through_structure_stargate(&ctx, &mut sess, &[PyRep::Integer(jb_a)])
            .await
            .unwrap_err()
            .code,
        "JumpBridgeRequiresSovNull"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.get_mut(&jb_b).unwrap().solar_system_id = sys_b;
    });

    // JumpBridgeOutOfRange (> 5.0 LY)
    let mut far_sess = sess.clone();
    far_sess.solar_system_id = Some(far_a);
    assert_eq!(
        handle_cmd_jump_through_structure_stargate(&ctx, &mut far_sess, &[PyRep::Integer(jb_far_a)])
            .await
            .unwrap_err()
            .code,
        "JumpBridgeOutOfRange"
    );

    // ShipNotAllowedThroughAnsiblex via Titan group_id
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&ship_id).unwrap().type_id = titan_tid;
    });
    assert_eq!(
        handle_cmd_jump_through_structure_stargate(&ctx, &mut sess, &[PyRep::Integer(jb_a)])
            .await
            .unwrap_err()
            .code,
        "ShipNotAllowedThroughAnsiblex"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&ship_id).unwrap().type_id = 587;
    });

    // ShipNotInSpace before ball spawned
    let _ = sys_a_pre.remove_ball(ship_id).await;
    let _ = far_a_pre.remove_ball(ship_id).await;
    assert_eq!(
        handle_cmd_jump_through_structure_stargate(&ctx, &mut sess, &[PyRep::Integer(jb_a)])
            .await
            .unwrap_err()
            .code,
        "ShipNotInSpace"
    );

    // Spawn ship ball: test ShipInWarp, WarpScrambled, ball mass > 1.48 Mt, OutsideJumpBridgeRange, SessionChangeInProgress
    let sys_a_h = ctx.systems.get_or_spawn(sys_a);
    let _ = sys_a_h
        .spawn_ball_with_combat(
            Ball {
                id: ship_id,
                type_id: 587,
                position: DVec3::new(10_500.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 2.0,
                mass: 1_000_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: MovementMode::new_warp(DVec3::ZERO, DVec3::new(1_000_000.0, 0.0, 0.0), 3.0),
            },
            CombatState::default(),
        )
        .await;
    assert_eq!(
        handle_cmd_jump_through_structure_stargate(&ctx, &mut sess, &[PyRep::Integer(jb_a)])
            .await
            .unwrap_err()
            .code,
        "ShipInWarp"
    );

    let _ = sys_a_h.set_movement_mode(ship_id, MovementMode::Stop).await;
    sys_a_h.try_update_combat_state(ship_id, |cs| {
        cs.warp_core_strength = 0;
        cs.warp_scramble_strength = 2;
    });
    assert_eq!(
        handle_cmd_jump_through_structure_stargate(&ctx, &mut sess, &[PyRep::Integer(jb_a)])
            .await
            .unwrap_err()
            .code,
        "WarpScrambled"
    );
    sys_a_h.try_update_combat_state(ship_id, |cs| {
        cs.warp_scramble_strength = 0;
    });

    // Ball mass > ANSIBLEX_MAX_MASS_KG
    {
        let mut bp = sys_a_h.ballpark.write().unwrap();
        bp.balls.get_mut(&ship_id).unwrap().mass = ANSIBLEX_MAX_MASS_KG + 100.0;
    }
    assert_eq!(
        handle_cmd_jump_through_structure_stargate(&ctx, &mut sess, &[PyRep::Integer(jb_a)])
            .await
            .unwrap_err()
            .code,
        "ShipNotAllowedThroughAnsiblex"
    );
    {
        let mut bp = sys_a_h.ballpark.write().unwrap();
        bp.balls.get_mut(&ship_id).unwrap().mass = 1_000_000.0;
    }

    // OutsideJumpBridgeRange
    let _ = sys_a_h
        .set_ball_position(ship_id, DVec3::new(100_000.0, 0.0, 0.0))
        .await;
    assert_eq!(
        handle_cmd_jump_through_structure_stargate(&ctx, &mut sess, &[PyRep::Integer(jb_a)])
            .await
            .unwrap_err()
            .code,
        "OutsideJumpBridgeRange"
    );
    let _ = sys_a_h
        .set_ball_position(ship_id, DVec3::new(10_500.0, 0.0, 0.0))
        .await;

    // SessionChangeInProgress
    crate::services::space_destiny::record_session_change_timer(
        &ctx,
        char_id,
        ctx.now_unix(),
        sys_a,
        0,
        "jump",
    );
    assert_eq!(
        handle_cmd_jump_through_structure_stargate(&ctx, &mut sess, &[PyRep::Integer(jb_a)])
            .await
            .unwrap_err()
            .code,
        "SessionChangeInProgress"
    );
    ctx.advance_secs(15);

    // Multi-stack Liquid Ozone depletion in SpecializedFuelBay (stack1 fully depleted & removed, stack2 partial)
    let oz_1 = 910_610_i64;
    let oz_2 = 910_611_i64;
    ctx.db.with_mut_snapshot(|snap| {
        for (oid, qty) in [(oz_1, 20), (oz_2, 500)] {
            snap.items.insert(
                oid,
                InventoryItemRecord {
                    item_id: oid,
                    type_id: LIQUID_OZONE_TYPE_ID,
                    owner_id: corp_id,
                    location_id: jb_a,
                    flag: InventoryFlag::SpecializedFuelBay,
                    quantity: qty,
                    singleton: false,
                    version: 1,
                    ..Default::default()
                },
            );
        }
    });
    let res = handle_cmd_jump_through_structure_stargate(&ctx, &mut sess, &[PyRep::Integer(jb_a)])
        .await
        .expect("jump succeeds with multi-stack SpecializedFuelBay ozone");
    let PyRep::KeyVal(kv) = res else { panic!() };
    assert_eq!(kv.get("solarSystemID").and_then(|v| v.as_i64()), Some(sys_b as i64));
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.items.contains_key(&oz_1));
        assert!(snap.items[&oz_2].quantity < 500);
    });
}
