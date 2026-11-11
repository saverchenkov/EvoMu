//! Unit tests for `structure_ansiblex_navigation` (null-sec check, linked jump bridges, and Ansiblex jump execution).

#![forbid(unsafe_code)]

use super::*;
use crate::services::account_services::account_services_tests::PyRepTestExt;
use crate::services::fleet_social::fleet_social_fleet_ops::fleet_social_fleet_ops_tests::{
    clone_char_with, test_ctx_with_clock,
};
use crate::services::station_structure::structure_upwell_ops::{load_ext_state, save_ext_state};
use eve_db::{InventoryItemRecord, UpwellStructureRecord};
use eve_destiny::{Ball, MovementMode};
use glam::DVec3;
use std::collections::HashMap;

#[tokio::test]
async fn ansiblex_linked_queries_and_jump_through_structure_stargate() {
    let ctx = test_ctx_with_clock(1_800_000_000);
    assert!(!is_sovereign_nullsec_for_ansiblex(&ctx.sde, 30_000_142));
    assert!(!is_sovereign_nullsec_for_ansiblex(&ctx.sde, 30_100_000));
    assert!(!is_sovereign_nullsec_for_ansiblex(&ctx.sde, 31_000_005));

    // Find two sovereign null-sec systems within 5.0 LY of each other
    let sov_systems: Vec<u32> = ctx
        .sde
        .solar_systems
        .keys()
        .copied()
        .filter(|&sid| is_sovereign_nullsec_for_ansiblex(&ctx.sde, sid))
        .take(80)
        .collect();
    let (sys_a, sys_b) = sov_systems
        .iter()
        .find_map(|&a| {
            sov_systems.iter().find_map(|&b| {
                let d = crate::services::space_destiny::space_nav_travel::distance_ly(&ctx, a, b);
                (a != b && d > 0.0 && d <= 4.5).then_some((a, b))
            })
        })
        .expect("pair of sov null systems within 4.5 LY");

    let char_id = 910_401_i64;
    let corp_id = 98_000_001_i64;
    let ship_id = 910_402_i64;
    let jb_a = 1_000_000_940_001_i64;
    let jb_b = 1_000_000_940_002_i64;
    let ozone_id = 910_403_i64;

    ctx.db.with_mut_snapshot(|snap| {
        let ch = clone_char_with(snap, char_id, "Ansiblex Pilot", corp_id, sys_a, ship_id);
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
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
        for (sid, sys) in [(jb_a, sys_a), (jb_b, sys_b)] {
            snap.structures.insert(
                sid,
                UpwellStructureRecord {
                    structure_id: sid,
                    type_id: ANSIBLEX_TYPE_ID,
                    owner_corp_id: corp_id,
                    solar_system_id: sys,
                    name: format!("Ansiblex {sid}"),
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
                },
            );
        }
        let mut ext = load_ext_state(snap);
        ext.jump_bridge_links.insert(jb_a, jb_b);
        ext.jump_bridge_links.insert(jb_b, jb_a);
        save_ext_state(snap, &ext);
    });

    let mut session = SessionState::new(1);
    session.user_id = Some(char_id as u64);
    session.character_id = Some(char_id);
    session.corporation_id = Some(corp_id);
    session.solar_system_id = Some(sys_a);
    session.ship_id = Some(ship_id);

    // GetLinkedStructure and GetJbStructureDestination
    let linked_rep = handle_get_linked_structure(&ctx, &mut session, &[PyRep::Integer(jb_a)]).unwrap();
    assert_eq!(
        linked_rep,
        PyRep::Tuple(vec![PyRep::Integer(jb_b), PyRep::Integer(sys_b as i64)])
    );
    assert_eq!(
        handle_get_linked_structure(&ctx, &mut session, &[PyRep::Integer(999_999)]).unwrap(),
        PyRep::None
    );
    assert_eq!(
        handle_get_jb_structure_destination(&ctx, &mut session, &[PyRep::Integer(jb_a)]).unwrap(),
        PyRep::Integer(sys_b as i64)
    );
    assert_eq!(
        handle_get_jb_structure_destination(&ctx, &mut session, &[PyRep::Integer(999_999)])
            .unwrap_err()
            .code,
        "NoLinkedStructure"
    );

    // GetJumpBridgesWithMyAccess and GetNearbyJumpBridges
    let PyRep::Tuple(access_tup) = handle_get_jump_bridges_with_my_access(&ctx, &mut session).unwrap() else {
        panic!("expected 3-tuple");
    };
    assert_eq!(access_tup.len(), 3);
    let PyRep::List(nearby) = handle_get_nearby_jump_bridges(&ctx, &mut session).unwrap() else {
        panic!("expected list");
    };
    assert_eq!(nearby.len(), 2);

    // Docked session rejected
    let mut docked_sess = session.clone();
    docked_sess.structure_id = Some(jb_a);
    assert_eq!(
        handle_cmd_jump_through_structure_stargate(&ctx, &mut docked_sess, &[PyRep::Integer(jb_a)])
            .await
            .unwrap_err()
            .code,
        "ShipDocked"
    );

    // Spawn ship ball next to jb_a; without Liquid Ozone -> InsufficientLiquidOzone
    let sys_a_handle = ctx.systems.get_or_spawn(sys_a);
    let _ = sys_a_handle
        .spawn_ball(Ball {
            id: ship_id,
            type_id: 587,
            position: DVec3::new(10_500.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 2.0,
            mass: 1_000_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;
    assert_eq!(
        handle_cmd_jump_through_structure_stargate(&ctx, &mut session, &[PyRep::Integer(jb_a)])
            .await
            .unwrap_err()
            .code,
        "InsufficientLiquidOzone"
    );

    // Add Liquid Ozone to jb_a StructureFuel bay and jump -> transitions to sys_b
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            ozone_id,
            InventoryItemRecord {
                item_id: ozone_id,
                type_id: LIQUID_OZONE_TYPE_ID,
                owner_id: corp_id,
                location_id: jb_a,
                flag: InventoryFlag::StructureFuel,
                quantity: 10_000,
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
    let jump_res = handle_cmd_jump_through_structure_stargate(&ctx, &mut session, &[PyRep::Integer(jb_a)])
        .await
        .expect("Ansiblex jump succeeds");
    let PyRep::KeyVal(kv) = jump_res else { panic!("expected keyval") };
    assert_eq!(kv.get("solarSystemID").and_then(|v| v.as_i64()), Some(sys_b as i64));
    assert_eq!(session.solar_system_id, Some(sys_b));
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.characters[&char_id].solar_system_id, sys_b);
        assert!(snap.items[&ozone_id].quantity < 10_000);
    });
}

#[tokio::test]
async fn ansiblex_error_branches_acl_mass_scramble_warp_and_multi_ozone_stacks() {
    let ctx = test_ctx_with_clock(1_800_000_000);

    // 1. is_sovereign_nullsec_for_ansiblex edge cases (unknown system)
    assert!(!is_sovereign_nullsec_for_ansiblex(&ctx.sde, 99_999_999));

    // Find two sov null systems within 5.0 LY, and one sov null system > 5.0 LY from sys_a
    let sov_systems: Vec<u32> = ctx
        .sde
        .solar_systems
        .keys()
        .copied()
        .filter(|&sid| is_sovereign_nullsec_for_ansiblex(&ctx.sde, sid))
        .take(200)
        .collect();
    let (sys_a, sys_b) = sov_systems
        .iter()
        .find_map(|&a| {
            sov_systems.iter().find_map(|&b| {
                let d = crate::services::space_destiny::space_nav_travel::distance_ly(&ctx, a, b);
                (a != b && d > 0.0 && d <= 4.5).then_some((a, b))
            })
        })
        .expect("pair of sov null systems within 4.5 LY");
    let sys_far = sov_systems
        .iter()
        .copied()
        .find(|&c| {
            crate::services::space_destiny::space_nav_travel::distance_ly(&ctx, sys_a, c) > 5.5
        })
        .expect("sov null system > 5.5 LY away");

    let char_id = 910_501_i64;
    let outsider_id = 910_502_i64;
    let corp_id = 98_000_001_i64;
    let other_corp_id = 98_000_099_i64;
    let ship_id = 910_503_i64;
    let jb_a = 1_000_000_950_001_i64;
    let jb_b = 1_000_000_950_002_i64;
    let jb_unlinked = 1_000_000_950_003_i64;
    let jb_destroyed = 1_000_000_950_004_i64;
    let jb_far = 1_000_000_950_005_i64;

    // Pick a Titan or Supercarrier type from SDE (group 30 or 659) to test mass/group rejection
    let supercap_tid = ctx
        .sde
        .types
        .values()
        .find(|t| matches!(t.group_id, 30 | 659))
        .map(|t| t.type_id)
        .unwrap_or(671);

    ctx.db.with_mut_snapshot(|snap| {
        let ch = clone_char_with(snap, char_id, "PilotA", corp_id, sys_a, ship_id);
        snap.characters.insert(char_id, ch);
        let outsider = clone_char_with(snap, outsider_id, "Outsider", other_corp_id, sys_a, 910_599);
        snap.characters.insert(outsider_id, outsider);

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
        for (sid, sys, state) in [
            (jb_a, sys_a, "ShieldVulnerable"),
            (jb_b, sys_b, "ShieldVulnerable"),
            (jb_unlinked, sys_a, "ShieldVulnerable"),
            (jb_destroyed, sys_b, "Destroyed"),
            (jb_far, sys_far, "ShieldVulnerable"),
        ] {
            snap.structures.insert(
                sid,
                UpwellStructureRecord {
                    structure_id: sid,
                    type_id: ANSIBLEX_TYPE_ID,
                    owner_corp_id: corp_id,
                    solar_system_id: sys,
                    name: format!("JB {sid}"),
                    position: [10_000.0, 0.0, 0.0],
                    state: state.into(),
                    quantum_core_installed: true,
                    fuel_blocks: 500,
                    shield_hp: 1.0,
                    armor_hp: 1.0,
                    hull_hp: 1.0,
                    reinforced_until_unix: 0,
                    last_damage_tick: 0,
                    last_damage_unix: 0,
                    self_destruct_at_unix: None,
                },
            );
        }
        let mut ext = load_ext_state(snap);
        ext.jump_bridge_links.insert(jb_a, jb_b);
        save_ext_state(snap, &ext);
    });

    // 2. Unauthenticated & missing argument errors
    let mut unauth = SessionState::new(99);
    assert!(handle_get_linked_structure(&ctx, &mut unauth, &[PyRep::Integer(jb_a)]).is_err());
    assert!(handle_get_jb_structure_destination(&ctx, &mut unauth, &[PyRep::Integer(jb_a)]).is_err());
    assert!(handle_get_jump_bridges_with_my_access(&ctx, &mut unauth).is_err());
    assert!(handle_get_nearby_jump_bridges(&ctx, &mut unauth).is_err());
    assert!(
        handle_cmd_jump_through_structure_stargate(&ctx, &mut unauth, &[PyRep::Integer(jb_a)])
            .await
            .is_err()
    );

    let mut sess = SessionState::new(1);
    sess.user_id = Some(char_id as u64);
    sess.character_id = Some(char_id);
    sess.corporation_id = Some(corp_id);
    sess.solar_system_id = Some(sys_a);
    sess.ship_id = Some(ship_id);

    assert_eq!(
        handle_get_linked_structure(&ctx, &mut sess, &[]).unwrap_err().code,
        "MissingStructureId"
    );
    assert_eq!(
        handle_get_jb_structure_destination(&ctx, &mut sess, &[]).unwrap_err().code,
        "MissingStructureId"
    );
    assert_eq!(
        handle_cmd_jump_through_structure_stargate(&ctx, &mut sess, &[])
            .await
            .unwrap_err()
            .code,
        "MissingStructureId"
    );

    // 3. ACL denial for outsider on recon queries & jump
    let mut out_sess = SessionState::new(2);
    out_sess.user_id = Some(outsider_id as u64);
    out_sess.character_id = Some(outsider_id);
    out_sess.corporation_id = Some(other_corp_id);
    out_sess.solar_system_id = Some(sys_a);
    out_sess.ship_id = Some(ship_id);
    assert_eq!(
        handle_get_linked_structure(&ctx, &mut out_sess, &[PyRep::Integer(jb_a)])
            .unwrap_err()
            .code,
        "AccessDenied"
    );
    assert_eq!(
        handle_get_jb_structure_destination(&ctx, &mut out_sess, &[PyRep::Integer(jb_a)])
            .unwrap_err()
            .code,
        "AccessDenied"
    );
    let sys_a_pre = ctx.systems.get_or_spawn(sys_a);
    let _ = sys_a_pre
        .spawn_ball(Ball {
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
        })
        .await;
    assert_eq!(
        handle_cmd_jump_through_structure_stargate(&ctx, &mut out_sess, &[PyRep::Integer(jb_a)])
            .await
            .unwrap_err()
            .code,
        "AccessDenied"
    );

    // 4. Unlinked accessible structure appears in accessibleStructureIDs but not pairs or nearby
    let PyRep::Tuple(acc_tup) = handle_get_jump_bridges_with_my_access(&ctx, &mut sess).unwrap() else {
        panic!("expected tuple");
    };
    assert_eq!(acc_tup[0].as_list().unwrap().len(), 1);
    assert_eq!(acc_tup[1].as_list().unwrap().len(), 4);

    // 5. Missing solar_system_id / ship_id / weapons timer / structure not found
    let mut no_sys_sess = sess.clone();
    no_sys_sess.solar_system_id = None;
    assert_eq!(
        handle_cmd_jump_through_structure_stargate(&ctx, &mut no_sys_sess, &[PyRep::Integer(jb_a)])
            .await
            .unwrap_err()
            .code,
        "NoSolarSystem"
    );
    let mut no_ship_sess = sess.clone();
    no_ship_sess.ship_id = None;
    assert_eq!(
        handle_cmd_jump_through_structure_stargate(&ctx, &mut no_ship_sess, &[PyRep::Integer(jb_a)])
            .await
            .unwrap_err()
            .code,
        "NoActiveShip"
    );

    // Weapons timer active
    ctx.crimewatch
        .write()
        .unwrap()
        .states
        .entry(char_id)
        .or_default()
        .weapon_timer_until_unix = ctx.now_unix() + 60;
    assert_eq!(
        handle_cmd_jump_through_structure_stargate(&ctx, &mut sess, &[PyRep::Integer(jb_a)])
            .await
            .unwrap_err()
            .code,
        "WeaponsTimerActive"
    );
    ctx.crimewatch
        .write()
        .unwrap()
        .states
        .get_mut(&char_id)
        .unwrap()
        .weapon_timer_until_unix = 0;

    // StructureNotFound & StructureNotOperational (wrong system / anchoring / no fuel / unlinked / dest offline / non-sov / far)
    assert_eq!(
        handle_cmd_jump_through_structure_stargate(&ctx, &mut sess, &[PyRep::Integer(999_999)])
            .await
            .unwrap_err()
            .code,
        "StructureNotFound"
    );
    assert_eq!(
        handle_cmd_jump_through_structure_stargate(&ctx, &mut sess, &[PyRep::Integer(jb_b)])
            .await
            .unwrap_err()
            .code,
        "StructureNotOperational"
    );
    assert_eq!(
        handle_cmd_jump_through_structure_stargate(&ctx, &mut sess, &[PyRep::Integer(jb_unlinked)])
            .await
            .unwrap_err()
            .code,
        "NoLinkedStructure"
    );

    // Link jb_a -> jb_destroyed -> resolve_linked_structure_id returns None -> NoLinkedStructure
    ctx.db.with_mut_snapshot(|snap| {
        let mut ext = load_ext_state(snap);
        ext.jump_bridge_links.insert(jb_a, jb_destroyed);
        save_ext_state(snap, &ext);
    });
    assert_eq!(
        handle_cmd_jump_through_structure_stargate(&ctx, &mut sess, &[PyRep::Integer(jb_a)])
            .await
            .unwrap_err()
            .code,
        "NoLinkedStructure"
    );

    // Link jb_a -> jb_b with jb_b offline (fuel_blocks = 0) -> StructureNotOperational
    ctx.db.with_mut_snapshot(|snap| {
        let mut ext = load_ext_state(snap);
        ext.jump_bridge_links.insert(jb_a, jb_b);
        save_ext_state(snap, &ext);
        snap.structures.get_mut(&jb_b).unwrap().fuel_blocks = 0;
    });
    assert_eq!(
        handle_cmd_jump_through_structure_stargate(&ctx, &mut sess, &[PyRep::Integer(jb_a)])
            .await
            .unwrap_err()
            .code,
        "StructureNotOperational"
    );

    // Restore jb_b fuel, link jb_a -> jb_far (> 5.0 LY) -> JumpBridgeOutOfRange
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.get_mut(&jb_b).unwrap().fuel_blocks = 500;
        let mut ext = load_ext_state(snap);
        ext.jump_bridge_links.insert(jb_a, jb_far);
        save_ext_state(snap, &ext);
    });
    assert_eq!(
        handle_cmd_jump_through_structure_stargate(&ctx, &mut sess, &[PyRep::Integer(jb_a)])
            .await
            .unwrap_err()
            .code,
        "JumpBridgeOutOfRange"
    );

    // Link jb_a -> jb_b in highsec (non-sov null) -> JumpBridgeRequiresSovNull
    ctx.db.with_mut_snapshot(|snap| {
        let mut ext = load_ext_state(snap);
        ext.jump_bridge_links.insert(jb_a, jb_b);
        save_ext_state(snap, &ext);
        snap.structures.get_mut(&jb_b).unwrap().solar_system_id = 30_000_142; // Jita highsec
    });
    assert_eq!(
        handle_cmd_jump_through_structure_stargate(&ctx, &mut sess, &[PyRep::Integer(jb_a)])
            .await
            .unwrap_err()
            .code,
        "JumpBridgeRequiresSovNull"
    );

    // Restore jb_b to sys_b; supercap ship item -> ShipNotAllowedThroughAnsiblex
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.get_mut(&jb_b).unwrap().solar_system_id = sys_b;
        snap.items.get_mut(&ship_id).unwrap().type_id = supercap_tid;
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

    // 6. Ballpark checks: ShipNotInSpace, ShipInWarp, WarpScrambled, OutsideJumpBridgeRange, SessionChangeInProgress
    let _ = sys_a_pre.remove_ball(ship_id).await;
    assert_eq!(
        handle_cmd_jump_through_structure_stargate(&ctx, &mut sess, &[PyRep::Integer(jb_a)])
            .await
            .unwrap_err()
            .code,
        "ShipNotInSpace"
    );

    let sys_a_handle = ctx.systems.get_or_spawn(sys_a);
    let _ = sys_a_handle
        .spawn_ball(Ball {
            id: ship_id,
            type_id: 587,
            position: DVec3::new(100_000.0, 0.0, 0.0), // far from jb_a
            velocity: DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 2.0,
            mass: 1_000_000.0,
            radius: 40.0,
            speed_fraction: 1.0,
            mode: MovementMode::Warp {
                origin: DVec3::new(100_000.0, 0.0, 0.0),
                destination: DVec3::ZERO,
                warp_speed_au_s: 3.0,
                total_distance: 100_000.0,
                phase: eve_destiny::WarpPhase::Cruising,
                elapsed_warp_time: 1.0,
            },
        })
        .await;
    assert_eq!(
        handle_cmd_jump_through_structure_stargate(&ctx, &mut sess, &[PyRep::Integer(jb_a)])
            .await
            .unwrap_err()
            .code,
        "ShipInWarp"
    );

    // Stop ship, scramble it -> WarpScrambled
    let _ = sys_a_handle.remove_ball(ship_id).await;
    let _ = sys_a_handle
        .spawn_ball(Ball {
            id: ship_id,
            type_id: 587,
            position: DVec3::new(100_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 2.0,
            mass: 1_000_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;
    let _ = sys_a_handle.set_warp_scramble_strength(ship_id, 2).await;
    assert_eq!(
        handle_cmd_jump_through_structure_stargate(&ctx, &mut sess, &[PyRep::Integer(jb_a)])
            .await
            .unwrap_err()
            .code,
        "WarpScrambled"
    );
    let _ = sys_a_handle.set_warp_scramble_strength(ship_id, 0).await;

    // OutsideJumpBridgeRange
    assert_eq!(
        handle_cmd_jump_through_structure_stargate(&ctx, &mut sess, &[PyRep::Integer(jb_a)])
            .await
            .unwrap_err()
            .code,
        "OutsideJumpBridgeRange"
    );

    // Move ship within range + active session change timer -> SessionChangeInProgress
    let _ = sys_a_handle.remove_ball(ship_id).await;
    let _ = sys_a_handle
        .spawn_ball(Ball {
            id: ship_id,
            type_id: 587,
            position: DVec3::new(10_500.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 2.0,
            mass: 1_000_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;
    crate::services::space_destiny::record_session_change_timer(
        &ctx,
        char_id,
        ctx.now_unix(),
        sys_a,
        0,
        "test_timer",
    );
    assert_eq!(
        handle_cmd_jump_through_structure_stargate(&ctx, &mut sess, &[PyRep::Integer(jb_a)])
            .await
            .unwrap_err()
            .code,
        "SessionChangeInProgress"
    );

    // Advance clock past 10s timer and split Liquid Ozone across 2 stacks (first small stack depleted & removed, second partially consumed)
    ctx.set_clock(std::sync::Arc::new(eve_db::clock::VirtualServerClock::with_fixed_base(
        1_800_000_020,
    )));
    ctx.db.with_mut_snapshot(|snap| {
        for (iid, flag, qty) in [
            (910_601_i64, InventoryFlag::StructureFuel, 20),
            (910_602_i64, InventoryFlag::SpecializedFuelBay, 5_000),
        ] {
            snap.items.insert(
                iid,
                InventoryItemRecord {
                    item_id: iid,
                    type_id: LIQUID_OZONE_TYPE_ID,
                    owner_id: corp_id,
                    location_id: jb_a,
                    flag,
                    quantity: qty,
                    singleton: false,
                    version: 1,
                    ..Default::default()
                },
            );
        }
    });
    let jump_ok = handle_cmd_jump_through_structure_stargate(&ctx, &mut sess, &[PyRep::Integer(jb_a)])
        .await
        .expect("jump succeeds after timer expires");
    assert_eq!(jump_ok.dict_get("solarSystemID"), Some(&PyRep::Integer(sys_b as i64)));
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.items.contains_key(&910_601), "first ozone stack fully depleted");
        assert!(snap.items[&910_602].quantity < 5_000);
    });
    let new_handle = ctx.systems.get_or_spawn(sys_b);
    let cs = new_handle.get_combat_state(ship_id).await.unwrap().unwrap();
    assert!(cs.is_cloaked);
    assert_eq!(cs.gate_cloak_remaining_ticks, 60);
    assert_eq!(cs.undock_invulnerable_remaining_ticks, 60);
}
