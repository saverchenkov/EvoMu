//! Semantic unit tests split from `space_entity_ops_extra_unit_tests`.

#![forbid(unsafe_code)]

use super::super::*;
use super::*;
use crate::services::combat_crimewatch::{
    with_mut_combat_runtime, ContainerLifetimeEntry, DroneSpaceState,
};
use eve_db::types::{FleetMemberRecord, FleetRecord, FleetRole};
use eve_db::{
    corp_roles, BookmarkRecord, CorporationRecord, InventoryFlag, InventoryItemRecord,
    UpwellStructureRecord,
};
use eve_destiny::{Ball, CombatState, MovementMode, TargetLock};
use eve_machonet::{service_roles, SessionState};
use eve_proto::{BridgeRpcError, PyRep};
use glam::DVec3;
use std::collections::HashMap;

#[tokio::test]
async fn beyonce_regroup_tag_target_and_goto_bookmark_validation_and_partial_warp() {
    let ctx = test_ctx();
    let sys_id = 30_000_142_u32;
    let char_id = 90_000_001_i64;
    let wing_mate = 90_000_002_i64;
    let other_wing_mate = 90_000_003_i64;
    let corp_id = 98_000_001_i64;
    let ship_id = 100_000_001_i64;
    let mut sess = make_session(char_id, corp_id, Some(sys_id), Some(ship_id), service_roles::ROLE_PLAYER_DEFAULT);

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    for (id, pos) in [
        (ship_id, DVec3::ZERO),
        (100_000_002, DVec3::new(5_000.0, 0.0, 0.0)),
        (100_000_003, DVec3::new(5_000.0, 0.0, 0.0)),
    ] {
        let _ = sys_handle.spawn_ball(make_ball(id, 587, pos)).await;
    }

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c2) = snap.characters.get_mut(&wing_mate) {
            c2.solar_system_id = sys_id;
            c2.active_ship_id = 100_000_002;
        }
        let mut c3 = snap.characters[&char_id].clone();
        c3.character_id = other_wing_mate;
        c3.solar_system_id = sys_id;
        c3.active_ship_id = 100_000_003;
        snap.characters.insert(other_wing_mate, c3);

        let mut mr = HashMap::new();
        mr.insert(
            char_id,
            FleetMemberRecord {
                character_id: char_id,
                role: FleetRole::WingCommander,
                wing_id: Some(10),
                squad_id: None,
                takes_fleet_warp: true,
                accepts_regroup: true,
                accepts_conduit_jumps: true,
                job: 0,
                role_name: String::new(),
                join_time_unix: 0,
            },
        );
        mr.insert(
            wing_mate,
            FleetMemberRecord {
                character_id: wing_mate,
                role: FleetRole::SquadMember,
                wing_id: Some(10),
                squad_id: Some(100),
                takes_fleet_warp: true,
                accepts_regroup: true,
                accepts_conduit_jumps: true,
                job: 0,
                role_name: String::new(),
                join_time_unix: 0,
            },
        );
        mr.insert(
            other_wing_mate,
            FleetMemberRecord {
                character_id: other_wing_mate,
                role: FleetRole::SquadMember,
                wing_id: Some(20),
                squad_id: Some(200),
                takes_fleet_warp: true,
                accepts_regroup: true,
                accepts_conduit_jumps: true,
                job: 0,
                role_name: String::new(),
                join_time_unix: 0,
            },
        );
        snap.fleets.insert(
            901,
            FleetRecord {
                fleet_id: 901,
                boss_id: 90_000_099,
                members: vec![char_id, wing_mate, other_wing_mate],
                member_details: mr,
                free_move: true,
                ..Default::default()
            },
        );
    });

    // CmdFleetRegroup by WingCommander only regroups wing_mate (wing 10), not other_wing_mate (wing 20)
    assert_eq!(call_ops(&ctx, &mut sess, "beyonce", "CmdFleetRegroup", &[]).await.unwrap(), PyRep::None);
    let (_, w1_ball) = sys_handle.get_ball_snapshot(100_000_002).await.unwrap().unwrap();
    let (_, w2_ball) = sys_handle.get_ball_snapshot(100_000_003).await.unwrap().unwrap();
    assert!(matches!(w1_ball.mode, MovementMode::Follow { target_id, .. } if target_id == ship_id));
    assert!(matches!(w2_ball.mode, MovementMode::Stop));

    // Lock both targets and tag with integer 5 -> re-tagging 100_000_003 replaces duplicate tag on 100_000_002
    for tid in [100_000_002, 100_000_003] {
        let _ = sys_handle
            .add_target_lock(
                ship_id,
                TargetLock {
                    target_id: tid,
                    lock_time_ms: 1000.0,
                    remaining_lock_ms: 0.0,
                    locked: true,
                    max_range_m: None,
                },
            )
            .await;
    }
    call_ops(
        &ctx,
        &mut sess,
        "beyonce",
        "CmdFleetTagTarget",
        &[PyRep::Integer(100_000_002), PyRep::Integer(5)],
    )
    .await
    .unwrap();
    call_ops(
        &ctx,
        &mut sess,
        "beyonce",
        "CmdFleetTagTarget",
        &[PyRep::Integer(100_000_003), PyRep::Integer(5)],
    )
    .await
    .unwrap();
    let fstate = call_ops(&ctx, &mut sess, "beyonce", "GetFleetState", &[]).await.unwrap();
    let PyRep::KeyVal(fkv) = fstate else { panic!("expected KeyVal") };
    assert_eq!(
        fkv.get("targetTags"),
        Some(&PyRep::Dict(vec![(PyRep::Integer(100_000_003), PyRep::String("5".into()))]))
    );

    // CmdGotoBookmark validation: docked, Abyssal, Zarzakh, expired, and NotEnoughCapacitor
    sess.station_id = Some(60_003_760);
    assert_eq!(
        call_ops(&ctx, &mut sess, "beyonce", "CmdGotoBookmark", &[PyRep::Integer(1)])
            .await
            .unwrap_err()
            .code,
        "NotInSpace"
    );
    sess.station_id = None;

    for (sys, expected) in [
        (32_000_001, "CannotWarpInAbyssalSpace"),
        (30_100_000, "DunBlacklistCannotWarp"),
    ] {
        sess.solar_system_id = Some(sys);
        assert_eq!(
            call_ops(&ctx, &mut sess, "beyonce", "CmdGotoBookmark", &[PyRep::Integer(1)])
                .await
                .unwrap_err()
                .code,
            expected
        );
    }
    sess.solar_system_id = Some(sys_id);

    let bm_id = 950_001_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.bookmarks.insert(
            bm_id,
            BookmarkRecord {
                bookmark_id: bm_id,
                owner_id: char_id,
                label: "Far Spot".into(),
                solar_system_id: sys_id,
                x: 500_000_000_000.0,
                y: 0.0,
                z: 0.0,
                item_id: None,
                memo: String::new(),
                folder_id: None,
                is_shared: false,
            },
        );
        snap.bookmark_expiries.insert(bm_id, 1);
    });
    assert_eq!(
        call_ops(&ctx, &mut sess, "beyonce", "CmdGotoBookmark", &[PyRep::Integer(bm_id)])
            .await
            .unwrap_err()
            .code,
        "BookmarkExpired"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.bookmark_expiries.remove(&bm_id);
    });

    let _ = sys_handle
        .spawn_ball_with_combat(
            make_ball(ship_id, 587, DVec3::ZERO),
            CombatState {
                capacitor: 0.0,
                max_capacitor: 500.0,
                ..Default::default()
            },
        )
        .await;
    assert_eq!(
        call_ops(&ctx, &mut sess, "beyonce", "CmdGotoBookmark", &[PyRep::Integer(bm_id)])
            .await
            .unwrap_err()
            .code,
        "NotEnoughCapacitor"
    );
}

#[tokio::test]
async fn beyonce_corp_asset_transfer_self_destruct_siphon_expiry_and_spawnpoint_branches() {
    let ctx = test_ctx();
    let sys_id = 30_000_142_u32;
    let char_id = 90_000_001_i64;
    let corp_id = 98_000_001_i64;
    let target_corp = 98_000_002_i64;
    let ship_id = 100_000_001_i64;
    let mut sess = make_session(char_id, corp_id, Some(sys_id), Some(ship_id), service_roles::ROLE_PLAYER_DEFAULT);
    sess.corp_role = corp_roles::ROLE_DIRECTOR;

    let item_id = 960_001_i64;
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let _ = sys_handle.spawn_ball(make_ball(ship_id, 587, DVec3::ZERO)).await;
    let _ = sys_handle.spawn_ball(make_ball(item_id, 34, DVec3::new(100.0, 0.0, 0.0))).await;
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.insert(
            target_corp,
            { let mut __corp = CorporationRecord::default();
                __corp.corporation_id = target_corp;
                __corp.name = "Target Corp".into();
                __corp.ticker = "TGT".into();
                __corp.ceo_id = 90_000_002;
                __corp },
        );
        snap.items.insert(
            item_id,
            InventoryItemRecord {
                item_id,
                type_id: 34,
                owner_id: corp_id,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });

    assert_eq!(
        call_ops(
            &ctx,
            &mut sess,
            "beyonce",
            "CmdTransferCorpAssetOwnership",
            &[PyRep::Integer(item_id), PyRep::Integer(1_000_035)]
        )
        .await
        .unwrap_err()
        .code,
        "InvalidTargetCorporation"
    );

    call_ops(
        &ctx,
        &mut sess,
        "beyonce",
        "CmdTransferCorpAssetOwnership",
        &[PyRep::Integer(item_id), PyRep::Integer(target_corp)],
    )
    .await
    .unwrap();
    assert_eq!(ctx.db.with_snapshot(|s| s.items[&item_id].owner_id), target_corp);

    assert_eq!(
        call_ops(&ctx, &mut sess, "beyonce", "GetCapacityOfSiphon", &[PyRep::Integer(item_id)])
            .await
            .unwrap(),
        PyRep::Real(0.0)
    );
    assert_eq!(
        call_ops(&ctx, &mut sess, "beyonce", "GetCapacityOfSiphon", &[PyRep::Integer(999_777_666)])
            .await
            .unwrap_err()
            .code,
        "ItemNotFound"
    );

    with_mut_combat_runtime(&ctx, |rt| {
        rt.container_lifetimes.insert(
            item_id,
            ContainerLifetimeEntry {
                item_id,
                solar_system_id: sys_id,
                type_id: 34,
                owner_id: corp_id,
                created_unix: 1_700_000_000,
                lifetime_secs: 7_200,
                is_abandoned: false,
            },
        );
    });
    let exp = call_ops(&ctx, &mut sess, "beyonce", "GetContainerExpiryDate", &[PyRep::Integer(item_id)])
        .await
        .unwrap();
    assert_eq!(exp.as_i64(), Some((1_700_007_200_i64 + 11_644_473_600_i64) * 10_000_000));
    assert_eq!(
        call_ops(&ctx, &mut sess, "beyonce", "GetContainerExpiryDate", &[PyRep::Integer(888_777_666)])
            .await
            .unwrap_err()
            .code,
        "ContainerNotFound"
    );

    let bm_site = 960_002_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.bookmarks.insert(
            bm_site,
            BookmarkRecord {
                bookmark_id: bm_site,
                owner_id: char_id,
                label: "Op Site".into(),
                solar_system_id: sys_id,
                x: 11.0,
                y: 22.0,
                z: 33.0,
                item_id: None,
                memo: String::new(),
                folder_id: None,
                is_shared: false,
            },
        );
    });
    let pos_rep = call_ops(&ctx, &mut sess, "beyonce", "GetOperationSpawnpointPosition", &[PyRep::Integer(bm_site)])
        .await
        .unwrap();
    assert_eq!(pos_rep, PyRep::Tuple(vec![PyRep::Real(11.0), PyRep::Real(22.0), PyRep::Real(33.0)]));
    assert_eq!(
        call_ops(&ctx, &mut sess, "beyonce", "GetOperationSpawnpointPosition", &[PyRep::Integer(888_999_111)])
            .await
            .unwrap_err()
            .code,
        "SiteNotFound"
    );
}
