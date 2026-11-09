//! Semantic unit tests split from `space_entity_mining_extra_unit_tests`.

#![forbid(unsafe_code)]

use super::super::*;
use super::*;
use crate::services::combat_crimewatch::with_combat_runtime;
use crate::services::space_entity_mining::handle;
use eve_db::types::TrainedSkillRecord;
use eve_db::{
    AsteroidRecord, CorporationRecord, InventoryFlag, InventoryItemRecord, IskCents,
    UpwellStructureRecord,
};
use eve_destiny::{Ball, CombatState, MovementMode};
use eve_proto::PyRep;
use eve_sde::{InvType, SdeRegistry, TypeMaterialEntry};
use glam::DVec3;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, OnceLock};

#[tokio::test]
async fn auto_discovered_drones_ownership_and_ice_drone_harvesting() {
    let mut ctx = test_ctx();
    let (sys_id, char_id, corp_id) = (30_000_142, 90_000_001, 98_000_001);
    let (ship_id, ice_drone_id, foreign_drone_id, ice_ast_id) =
        (1_091_000_001, 1_091_000_010, 1_091_000_011, 1_091_000_020);

    upsert_type(&mut ctx.sde, 997_001, 25, 6, "Test Ship", 5_000.0, 5_000.0, 1, &[]);
    upsert_type(
        &mut ctx.sde,
        997_002,
        101,
        18,
        "Ice Harvesting Drone II",
        10.0,
        0.0,
        1,
        &[
            (77, 1_000.0),
            (73, 60.0),
            (37, 250.0),
            (3154, 50.0),
            (182, 43702.0),
            (183, 43703.0),
        ],
    );
    upsert_type(&mut ctx.sde, 997_003, 465, 25, "Glacial Mass", 1_000.0, 0.0, 1, &[]);

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            ship_id,
            make_item(ship_id, 997_001, char_id, sys_id as i64, InventoryFlag::None, 1),
        );
        snap.items.insert(
            ice_drone_id,
            make_item(ice_drone_id, 997_002, char_id, sys_id as i64, InventoryFlag::None, 1),
        );
        snap.items.insert(
            foreign_drone_id,
            make_item(foreign_drone_id, 997_002, 90_000_999, sys_id as i64, InventoryFlag::None, 1),
        );
        snap.asteroids.insert(
            ice_ast_id,
            AsteroidRecord {
                asteroid_id: ice_ast_id,
                type_id: 997_003,
                solar_system_id: sys_id,
                belt_id: 1,
                quantity: 50,
                initial_quantity: 50,
                position: (1_000.0, 0.0, 0.0),
                radius: 50.0,
            },
        );
        let sk = snap.skills.entry(char_id).or_default();
        sk.insert(
            43702,
            TrainedSkillRecord {
                type_id: 43702,
                level: 5,
                skill_points: 256_000.0,
            },
        );
        sk.insert(
            43703,
            TrainedSkillRecord {
                type_id: 43703,
                level: 4,
                skill_points: 100_000.0,
            },
        );
        sk.insert(
            3436,
            TrainedSkillRecord {
                type_id: 3436,
                level: 5,
                skill_points: 256_000.0,
            },
        );
    });

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let _ = sys_handle
        .spawn_ball_with_combat(
            make_ship_ball(ship_id, 997_001, DVec3::ZERO),
            CombatState::default(),
        )
        .await;

    let mut sess = make_session(char_id, corp_id, Some(sys_id), Some(ship_id));

    // Foreign drone -> InvalidDroneOwner
    let owner_err = handle(
        &ctx,
        &mut sess,
        "entity",
        "CmdMineRepeatedly",
        &[PyRep::Integer(foreign_drone_id), PyRep::Integer(ice_ast_id)],
        None,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(owner_err.code, "InvalidDroneOwner");

    // Single arg (ice_ast_id) auto-discovers launched ice_drone_id in space!
    let ok = handle(
        &ctx,
        &mut sess,
        "entity",
        "CmdMineRepeatedly",
        &[PyRep::Integer(ice_ast_id)],
        None,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(ok, PyRep::Dict(Vec::new()));

    let st = with_combat_runtime(&ctx, |rt| rt.active_drone_miners.get(&ice_drone_id).cloned())
        .expect("ice drone miner state");
    assert_eq!(st.asteroid_id, ice_ast_id);
    assert!((st.waste_prob - 0.50).abs() < 1e-6);
    assert_eq!(st.destination_flag, InventoryFlag::Cargo);
}

#[tokio::test]
async fn reprocessing_npc_station_corp_hangar_locks_singleton_and_insufficient_funds() {
    let mut ctx = test_ctx();
    let (sys_id, char_id, corp_id) = (30_000_142, 90_000_001, 98_000_001);
    let station_id = 60_003_760_i64; // Jita IV - Moon 4
    let (
        corp_ore_id,
        singleton_id,
        locked_id,
        wrong_loc_id,
        ship_item_id,
    ) = (
        1_092_000_010,
        1_092_000_011,
        1_092_000_012,
        1_092_000_013,
        1_092_000_014,
    );

    upsert_type(&mut ctx.sde, 998_001, 462, 25, "TaxedOre", 0.1, 0.0, 100, &[]);
    Arc::make_mut(&mut ctx.sde).type_materials.insert(
        998_001,
        vec![TypeMaterialEntry {
            material_type_id: 34,
            quantity: 10_000,
        }],
    );

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.corporation_id = corp_id;
            ch.active_ship_id = ship_item_id;
            ch.balance = IskCents(0);
        }
        snap.corporations.insert(
            corp_id,
            { let mut __corp = CorporationRecord::default();
                __corp.corporation_id = corp_id;
                __corp.name = "Mining Corp".into();
                __corp.ticker = "MINE".into();
                __corp.ceo_id = char_id;
                __corp.members = HashSet::from([char_id]);
                __corp.member_roles = HashMap::from([(char_id, eve_db::corp_roles::ROLE_DIRECTOR)]);
                __corp },
        );
        snap.items.insert(
            corp_ore_id,
            make_item(corp_ore_id, 998_001, corp_id, station_id, InventoryFlag::CorpSAG1, 200),
        );
        let mut s_item = make_item(singleton_id, 998_001, char_id, station_id, InventoryFlag::Hangar, 200);
        s_item.singleton = true;
        snap.items.insert(singleton_id, s_item);
        snap.items.insert(
            locked_id,
            make_item(locked_id, 998_001, char_id, station_id, InventoryFlag::Locked, 200),
        );
        snap.items.insert(
            wrong_loc_id,
            make_item(wrong_loc_id, 998_001, char_id, station_id + 1, InventoryFlag::Hangar, 200),
        );
        snap.items.insert(
            ship_item_id,
            make_item(ship_item_id, 998_001, char_id, station_id, InventoryFlag::Hangar, 200),
        );
    });

    let mut sess = make_session(char_id, corp_id, Some(sys_id), Some(ship_item_id));
    sess.corp_role = eve_db::corp_roles::ROLE_DIRECTOR;

    // 1. Station without reprocessing service (e.g. non-existent station 60_999_999)
    sess.station_id = Some(60_999_999);
    assert_eq!(
        handle(&ctx, &mut sess, "reprocessingSvc", "Reprocess", &[PyRep::Integer(corp_ore_id)], None)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "StationServiceUnavailable"
    );
    assert_eq!(
        handle(&ctx, &mut sess, "reprocessingSvc", "GetQuotes", &[], None)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "StationServiceUnavailable"
    );

    // 2. Switch to Jita 4-4
    sess.station_id = Some(station_id);
    assert_eq!(
        handle(&ctx, &mut sess, "reprocessingSvc", "Reprocess", &[], None)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "InvalidArgument"
    );
    assert_eq!(
        handle(&ctx, &mut sess, "reprocessingSvc", "Reprocess", &[PyRep::Integer(999_999_999)], None)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "ItemNotFound"
    );
    assert_eq!(
        handle(&ctx, &mut sess, "reprocessingSvc", "Reprocess", &[PyRep::Integer(wrong_loc_id)], None)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "OwnershipOrLocationViolation"
    );
    assert_eq!(
        handle(&ctx, &mut sess, "reprocessingSvc", "Reprocess", &[PyRep::Integer(ship_item_id)], None)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "CannotReprocessActiveShip"
    );
    assert_eq!(
        handle(&ctx, &mut sess, "reprocessingSvc", "Reprocess", &[PyRep::Integer(singleton_id)], None)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "ItemMustBeRepackaged"
    );
    assert_eq!(
        handle(&ctx, &mut sess, "reprocessingSvc", "Reprocess", &[PyRep::Integer(locked_id)], None)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "OwnershipOrLocationViolation"
    );

    // 3. InsufficientFunds for NPC station reprocessing tax
    assert_eq!(
        handle(&ctx, &mut sess, "reprocessingSvc", "Reprocess", &[PyRep::Integer(corp_ore_id)], None)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "InsufficientFunds"
    );

    // 4. Fund character and run batch Reprocess ([corp_ore_id]) + single-arg GetQuotes
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().balance = IskCents(500_000_000);
    });
    let quotes = handle(
        &ctx,
        &mut sess,
        "reprocessingSvc",
        "GetQuotes",
        &[PyRep::List(vec![PyRep::Integer(corp_ore_id), PyRep::Integer(999_999)])],
        None,
    )
    .await
    .unwrap()
    .unwrap();
    let PyRep::Tuple(ref q_parts) = quotes else {
        panic!("expected 3-tuple from 1-arg GetQuotes, got {quotes:?}");
    };
    assert_eq!(q_parts.len(), 3);
    let PyRep::Dict(ref q_entries) = q_parts[2] else {
        panic!("expected rawQuotes Dict, got {:?}", q_parts[2]);
    };
    assert_eq!(q_entries.len(), 1);
    assert_eq!(q_entries[0].0, PyRep::Integer(corp_ore_id));

    let batch_rep = handle(
        &ctx,
        &mut sess,
        "reprocessingSvc",
        "Reprocess",
        &[PyRep::List(vec![PyRep::Integer(corp_ore_id)])],
        None,
    )
    .await
    .unwrap()
    .unwrap();
    let PyRep::List(batch_list) = batch_rep else {
        panic!("expected batch list");
    };
    assert_eq!(batch_list.len(), 1);
    assert!(
        batch_list[0]
            .dict_get("reprocessingTaxISK")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0)
            > 0.0
    );

    // 5. Structure with fuel_blocks == 0 -> ReprocessingServiceOffline
    let unfueled_st = 1_092_000_999_000_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            unfueled_st,
            UpwellStructureRecord {
                structure_id: unfueled_st,
                type_id: 35_832,
                owner_corp_id: corp_id,
                solar_system_id: sys_id,
                name: "Unfueled".into(),
                fuel_blocks: 0,
                ..Default::default()
            },
        );
    });
    sess.station_id = None;
    sess.structure_id = Some(unfueled_st);
    assert_eq!(
        handle(&ctx, &mut sess, "reprocessingSvc", "Reprocess", &[PyRep::Integer(singleton_id)], None)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "ReprocessingServiceOffline"
    );
}
