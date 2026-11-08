//! Semantic unit tests split from `space_cyno_filaments_tests`.

#![forbid(unsafe_code)]

use super::*;

use crate::router::BoundObjectEntry;
use crate::services::mobile_deployables::{
    write_deployable_state_snap, MobileDeployableRecord,
};
use eve_db::types::FleetRecord;
use eve_db::InventoryFlag;
use eve_proto::PyRep;
use glam::DVec3;
use std::collections::HashMap;

#[tokio::test]
async fn random_jump_filament_teleports_fleet_away_from_stargates() {
    let mut ctx = test_ctx();
    let cur_sys = 30000142;
    let char_id = 95_301;
    let ship_id = 77_301;
    let filament_tid = 99_620;
    // Attribute 3025 = filamentSystemDistributionID: 4041 = Noise/Needlejack Null-Sec
    clone_type_with(
        &mut ctx.sde,
        58958,
        filament_tid,
        4041,
        30,
        0.1,
        0.0,
        &[(3025, 4041.0)],
    );

    let fil_item_id = ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().cloned().expect("demo char");
        ch.character_id = char_id;
        ch.solar_system_id = cur_sys;
        ch.active_ship_id = ship_id;
        ch.station_id = None;
        ch.structure_id = None;
        snap.characters.insert(char_id, ch);
        snap.fleets.insert(
            7301,
            FleetRecord {
                fleet_id: 7301,
                boss_id: char_id,
                members: vec![char_id],
                wings: vec![],
                squads: vec![],
                free_move: true,
                is_registered: false,
                ..Default::default()
            },
        );
        put_item(snap, ship_id, 588, char_id, i64::from(cur_sys), InventoryFlag::Hangar, 1, true);
        put_item(
            snap,
            77_302,
            filament_tid,
            char_id,
            ship_id,
            InventoryFlag::Cargo,
            2,
            false,
        )
    });
    let sys_h = ctx.systems.get_or_spawn(cur_sys);
    // Safe deep-space coordinate > 1,000,000 m from all gates/stations
    spawn_ship(
        &sys_h,
        ship_id,
        588,
        25,
        6,
        DVec3::new(1.5e13, -2.1e13, 0.9e13),
        DVec3::ZERO,
        false,
    )
    .await;

    let mut sess = make_session(char_id, 98000001, Some(cur_sys), Some(ship_id));
    let kw = HashMap::new();
    let res = handle(
        &ctx,
        &mut sess,
        None,
        "randomJumpMgr",
        "ActivateRandomJumpFilament",
        &[PyRep::Integer(fil_item_id)],
        &kw,
    )
    .await
    .expect("handled")
    .expect("filament ok");

    let PyRep::KeyVal(kv) = res else {
        panic!("expected KeyVal");
    };
    let new_sys = kv.get("solarSystemID").and_then(|v| v.as_i64()).expect("destination system id") as u32;
    assert_ne!(new_sys, cur_sys);
    assert_eq!(sess.solar_system_id, Some(new_sys));
    let rem_qty = ctx.db.get_item(fil_item_id).map(|i| i.quantity).unwrap_or(0);
    assert_eq!(rem_qty, 1);
}

#[tokio::test]
async fn cone_scan_filters_angles_inhibitors_cloaks_and_docks() {
    let ctx = test_ctx();
    let sys_id = 30000142;
    let char_id = 95_401;
    let caller_ship = 77_401;
    let in_cone_ship = 77_402;
    let out_cone_ship = 77_403;
    let cloaked_ship = 77_404;
    let inhibited_ship = 77_405;
    let inhibitor_id = 77_406;

    let sys_h = ctx.systems.get_or_spawn(sys_id);
    // Caller moving +X, visible +X target, behind -X target, cloaked +X target, inhibitor + hidden ship
    spawn_ship(&sys_h, caller_ship, 588, 25, 6, DVec3::ZERO, DVec3::new(200.0, 0.0, 0.0), false).await;
    spawn_ship(&sys_h, in_cone_ship, 588, 25, 6, DVec3::new(1_000_000.0, 0.0, 0.0), DVec3::ZERO, false).await;
    spawn_ship(&sys_h, out_cone_ship, 588, 25, 6, DVec3::new(-1_000_000.0, 0.0, 0.0), DVec3::ZERO, false).await;
    spawn_ship(&sys_h, cloaked_ship, 588, 25, 6, DVec3::new(1_200_000.0, 0.0, 0.0), DVec3::ZERO, true).await;
    spawn_ship(
        &sys_h,
        inhibitor_id,
        crate::services::mobile_deployables_ext::TYPE_MOBILE_SCAN_INHIBITOR,
        crate::services::mobile_deployables_ext::GROUP_MOBILE_SCAN_INHIBITOR,
        22,
        DVec3::new(5_000_000.0, 0.0, 0.0),
        DVec3::ZERO,
        false,
    )
    .await;
    spawn_ship(&sys_h, inhibited_ship, 588, 25, 6, DVec3::new(5_005_000.0, 0.0, 0.0), DVec3::ZERO, false).await;

    let now_u = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        write_deployable_state_snap(snap, |st| {
            st.deployables.insert(
                inhibitor_id,
                MobileDeployableRecord {
                    item_id: inhibitor_id,
                    type_id: crate::services::mobile_deployables_ext::TYPE_MOBILE_SCAN_INHIBITOR,
                    group_id: crate::services::mobile_deployables_ext::GROUP_MOBILE_SCAN_INHIBITOR,
                    owner_id: 9002,
                    solar_system_id: sys_id,
                    anchor_start_unix: now_u.saturating_sub(120),
                    anchor_duration_secs: 60,
                    anchor_completed: true,
                    is_unanchored: false,
                    expires_at_unix: now_u + 3600,
                    last_interaction_unix: now_u,
                    reinforced_until_unix: None,
                    last_ping_unix: None,
                    position: [5_000_000.0, 0.0, 0.0],
                    deployed_fleet_id: None,
                },
            );
        });
    });
    let balls = sys_h.list_balls().await.unwrap_or_default();
    assert_eq!(active_scan_inhibitor_positions(&ctx, sys_id, &balls).len(), 1);

    let mut sess = make_session(char_id, 98000001, Some(sys_id), Some(caller_ship));
    let bound = BoundObjectEntry {
        bound_id: 1,
        session_id: sess.session_id,
        user_id: 30,
        character_id: char_id,
        service: "scanMgr".into(),
        object_id: sys_id as i64,
        oid_str: format!("N=1:{sys_id}"),
        ref_id: 1,
    };
    let kw = HashMap::new();

    // Pass 0.5236 radians (~30 deg)
    let res = handle(
        &ctx,
        &mut sess,
        Some(&bound),
        "scanMgr",
        "ConeScan",
        &[PyRep::Real(0.5236), PyRep::Real(14.3 * 149_597_870_700.0)],
        &kw,
    )
    .await
    .expect("handled")
    .expect("ConeScan ok");

    let PyRep::List(list) = res else {
        panic!("expected List");
    };
    let hit_ids: Vec<i64> = list
        .iter()
        .filter_map(|r| match r {
            PyRep::KeyVal(kv) => kv.get("id").and_then(PyRep::as_i64),
            _ => None,
        })
        .collect();
    assert!(hit_ids.contains(&in_cone_ship));
    assert!(hit_ids.contains(&inhibitor_id));
    assert!(!hit_ids.contains(&out_cone_ship));
    assert!(!hit_ids.contains(&cloaked_ship));
    assert!(!hit_ids.contains(&inhibited_ship));

    // Docked session cannot d-scan
    let mut docked = sess;
    docked.station_id = Some(60003760);
    let err_res = handle(
        &ctx,
        &mut docked,
        Some(&bound),
        "scanMgr",
        "ConeScan",
        &[PyRep::Real(360.0), PyRep::Real(1.0e12)],
        &kw,
    )
    .await
    .expect("handled");
    assert!(err_res.is_err());
}

#[tokio::test]
async fn fleet_beacon_jump_rejects_highsec_and_non_fleet_targets() {
    let ctx = test_ctx();
    let sys_id = 30000142;
    let boss_id = 95_501;
    let member_id = 95_502;
    let boss_ship = 77_501;
    let member_ship = 77_502;

    let sys_h = ctx.systems.get_or_spawn(sys_id);
    spawn_ship(&sys_h, boss_ship, 588, 25, 6, DVec3::new(9_000_000.0, 4_000_000.0, 0.0), DVec3::ZERO, false).await;
    spawn_ship(&sys_h, member_ship, 588, 25, 6, DVec3::ZERO, DVec3::ZERO, false).await;

    ctx.db.with_mut_snapshot(|snap| {
        put_item(snap, boss_ship, 588, boss_id, i64::from(sys_id), InventoryFlag::Hangar, 1, true);
        put_item(snap, member_ship, 588, member_id, i64::from(sys_id), InventoryFlag::Hangar, 1, true);
    });

    let mut member_sess = make_session(member_id, 98000001, Some(sys_id), Some(member_ship));
    let kw = HashMap::new();
    // rpc-lint: allow live crucible:src/eve-server/ship/BeyonceService.h CmdBeaconJumpFleet
    let non_fleet_err = handle(&ctx, &mut member_sess, None, "beyonce", "CmdBeaconJumpFleet", &[PyRep::Integer(boss_id), PyRep::Integer(boss_ship), PyRep::Integer(i64::from(sys_id))], &kw)
        .await
        .expect("handled")
        .unwrap_err();
    assert_eq!(non_fleet_err.code, "FleetNotInFleet");

    ctx.db.with_mut_snapshot(|snap| {
        snap.fleets.insert(
            7501,
            FleetRecord {
                fleet_id: 7501,
                boss_id,
                members: vec![boss_id, member_id],
                wings: vec![],
                squads: vec![],
                free_move: true,
                is_registered: false,
                ..Default::default()
            },
        );
    });
    // rpc-lint: allow live crucible:src/eve-server/ship/BeyonceService.h CmdBeaconJumpFleet
    let hs_err = handle(&ctx, &mut member_sess, None, "beyonce", "CmdBeaconJumpFleet", &[PyRep::Integer(boss_id), PyRep::Integer(boss_ship), PyRep::Integer(i64::from(sys_id))], &kw)
        .await
        .expect("handled")
        .unwrap_err();
    assert_eq!(hs_err.code, "CannotJumpToHighSecBeacon");
}

#[tokio::test]
async fn random_jump_filament_queues_set_state_and_valid_nullsec_ess_slim_items() {
    let mut ctx = test_ctx();
    let cur_sys = 30_000_142_u32;
    let char_id = 95_601_i64;
    let ship_id = 77_601_i64;
    let filament_tid = 99_621_u32;
    clone_type_with(
        &mut ctx.sde,
        58958,
        filament_tid,
        4041,
        30,
        0.1,
        0.0,
        &[(3025, 4041.0)],
    );

    let fil_item_id = ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().cloned().expect("demo char");
        ch.character_id = char_id;
        ch.solar_system_id = cur_sys;
        ch.active_ship_id = ship_id;
        ch.station_id = None;
        ch.structure_id = None;
        snap.characters.insert(char_id, ch);
        snap.fleets.insert(
            7601,
            FleetRecord {
                fleet_id: 7601,
                boss_id: char_id,
                members: vec![char_id],
                wings: vec![],
                squads: vec![],
                free_move: true,
                is_registered: false,
                ..Default::default()
            },
        );
        put_item(snap, ship_id, 588, char_id, i64::from(cur_sys), InventoryFlag::Hangar, 1, true);
        put_item(
            snap,
            77_602,
            filament_tid,
            char_id,
            ship_id,
            InventoryFlag::Cargo,
            1,
            false,
        )
    });
    let sys_h = ctx.systems.get_or_spawn(cur_sys);
    spawn_ship(
        &sys_h,
        ship_id,
        588,
        25,
        6,
        DVec3::new(1.5e13, -2.1e13, 0.9e13),
        DVec3::ZERO,
        false,
    )
    .await;

    let mut sess = make_session(char_id, 98_000_001, Some(cur_sys), Some(ship_id));
    let kw = HashMap::new();
    let res = handle(
        &ctx,
        &mut sess,
        None,
        "randomJumpMgr",
        "ActivateRandomJumpFilament",
        &[PyRep::Integer(fil_item_id)],
        &kw,
    )
    .await
    .expect("handled")
    .expect("filament ok");

    let PyRep::KeyVal(kv) = res else {
        panic!("expected KeyVal");
    };
    let dest_sys = kv
        .get("solarSystemID")
        .and_then(|v| v.as_i64())
        .expect("destination system id") as u32;
    assert_ne!(dest_sys, cur_sys);
    let dest_h = ctx.systems.get_or_spawn(dest_sys);

    // 1. Destination ballpark must immediately have `SetState` queued at index 0 for the jumped ship
    let (_tick, obs_evs, _updates) = dest_h
        .drain_observer_updates(ship_id)
        .await
        .expect("drain observer updates");
    assert!(
        matches!(
            obs_evs.first(),
            Some(eve_destiny::DestinyEvent::SetState { observer_ball_id }) if *observer_ball_id == ship_id
        ),
        "expected SetState at index 0 immediately after ActivateRandomJumpFilament, got {obs_evs:?}"
    );
    assert!(
        ctx.db
            .with_snapshot(|snap| snap.item_positions.get(&ship_id).copied())
            .is_some(),
        "expected ship item_positions to be updated after filament jump"
    );

    // 2. Nullsec ESS query seeds ESS beacon + gate with valid non-Ship SlimItemMeta
    let _ = crate::services::space_destiny::handle(
        &ctx,
        &mut sess,
        None,
        "essMgr",
        "GetDataForClientSolarSystem",
        &[],
        &kw,
    )
    .await
    .expect("essMgr handled")
    .expect("essMgr ok");

    let bp = dest_h.ballpark.read().unwrap();
    let visible = bp.visible_balls_for_observer(ship_id);
    assert!(!visible.is_empty(), "expected visible destination balls");
    for ball in visible {
        let meta = eve_destiny::wire::resolve_slim_item_meta(Some(&bp), ball);
        let slim = eve_destiny::wire::slim_item_dict(ball, &meta);
        let tid = slim.dict_get("typeID").and_then(|v| v.as_i64()).unwrap_or(0);
        let gid = slim.dict_get("groupID").and_then(|v| v.as_i64()).unwrap_or(0);
        let cid = slim.dict_get("categoryID").and_then(|v| v.as_i64()).unwrap_or(0);
        assert!(tid > 0 && gid > 0 && cid > 0, "invalid slim item for ball {}: {:?}", ball.id, slim);
        if matches!(ball.mode, eve_destiny::MovementMode::Rigid) {
            assert_ne!(cid, 6, "rigid ball {} ({tid}) must not be misclassified as category Ship", ball.id);
        }
        if ball.id == crate::services::pve_ess_reserve_banks::ESS_BEACON_ID_BASE + i64::from(dest_sys) {
            assert_eq!((tid, gid, cid), (55914, 4079, 2));
        }
        if ball.id == crate::services::pve_ess_reserve_banks::ESS_GATE_ID_BASE + i64::from(dest_sys) {
            assert_eq!((tid, gid, cid), (17831, 366, 2));
        }
    }
    drop(bp);

    let (_st_tick, set_state_rep, _up) = dest_h.get_set_state(ship_id).await.expect("get_set_state");
    assert!(matches!(set_state_rep, PyRep::KeyVal(_)));
}
