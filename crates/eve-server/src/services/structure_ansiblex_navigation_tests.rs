//! Unit tests for `structure_ansiblex_navigation` (null-sec check, linked jump bridges, and Ansiblex jump execution).

#![forbid(unsafe_code)]

use super::*;
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
