//! Semantic unit tests split from `space_cyno_filaments_extra_unit_tests`.

#![forbid(unsafe_code)]

use super::super::*;
use super::*;
use crate::services::mobile_deployables::{write_deployable_state_snap, MobileDeployableRecord};
use crate::services::space_destiny::space_cyno_filaments::{
    active_scan_inhibitor_positions, handle,
};
use eve_db::types::{FleetRecord, UpwellStructureRecord};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use glam::DVec3;
use std::collections::HashMap;

#[tokio::test]
async fn structure_beacon_validation_fleet_beacon_jump_and_dscan_inhibitor_recon_immunity() {
    let mut ctx = test_ctx();
    clone_type_with(
        &mut ctx.sde,
        11_957,
        906,
        6,
        "Falcon Combat Recon",
        10_000_000.0,
        &[],
    );
    let sys_id = 30_000_142_u32;
    let char_id = 90_000_001_i64;
    let corp_id = 98_000_001_i64;
    let ship_id = 100_000_001_i64;
    let mut sess = SessionState::new(803);
    sess.user_id = Some(1);
    sess.character_id = Some(char_id);
    sess.corporation_id = Some(corp_id);
    sess.solar_system_id = Some(sys_id);
    sess.ship_id = Some(ship_id);

    // 1. Structure Cyno Beacon validation errors: BeaconNotFound, InvalidStructureBeacon, BeaconDestroyed, BeaconOutOfFuel
    let jita_handle = ctx.systems.get_or_spawn(sys_id);
    spawn_ship(&jita_handle, ship_id, 587, 25, 6, DVec3::ZERO, 1_000_000.0).await;
    let beacon_id = 1_000_000_910_001_i64;
    assert_eq!(
        call_cyno(
            &ctx,
            &mut sess,
            "structureCynoBeaconMgr",
            "CmdJumpToStructureBeacon",
            &[PyRep::Integer(beacon_id), PyRep::Integer(sys_id as i64)]
        )
        .await
        .unwrap_err()
        .code,
        "BeaconNotFound"
    );

    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            beacon_id,
            UpwellStructureRecord {
                structure_id: beacon_id,
                solar_system_id: sys_id,
                owner_corp_id: corp_id,
                type_id: 35832, // wrong type (not 35840)
                name: "Wrong Type".into(),
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 100,
                ..Default::default()
            },
        );
    });
    assert_eq!(
        call_cyno(
            &ctx,
            &mut sess,
            "structureCynoBeaconMgr",
            "CmdJumpToStructureBeacon",
            &[PyRep::Integer(sys_id as i64), PyRep::Integer(beacon_id)]
        )
        .await
        .unwrap_err()
        .code,
        "InvalidStructureBeacon"
    );

    ctx.db.with_mut_snapshot(|snap| {
        let st = snap.structures.get_mut(&beacon_id).unwrap();
        st.type_id = 35_840;
        st.state = "Destroyed".into();
    });
    assert_eq!(
        call_cyno(
            &ctx,
            &mut sess,
            "structureCynoBeaconMgr",
            "CmdBridgeToStructureBeacon",
            &[PyRep::Integer(sys_id as i64), PyRep::Integer(beacon_id)]
        )
        .await
        .unwrap_err()
        .code,
        "BeaconDestroyed"
    );

    ctx.db.with_mut_snapshot(|snap| {
        let st = snap.structures.get_mut(&beacon_id).unwrap();
        st.state = "ShieldVulnerable".into();
        st.fuel_blocks = 0;
    });
    assert_eq!(
        call_cyno(
            &ctx,
            &mut sess,
            "structureCynoBeaconMgr",
            "CmdGroupJumpToStructureBeacon",
            &[PyRep::Integer(sys_id as i64), PyRep::Integer(beacon_id)]
        )
        .await
        .unwrap_err()
        .code,
        "BeaconOutOfFuel"
    );

    // 2. CmdJumpToStructureBeacon from Wormhole Space: JumpDriveForbiddenInWormholeSpace
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.corporation_id = corp_id;
        }
        let st = snap.structures.get_mut(&beacon_id).unwrap();
        st.fuel_blocks = 100;
    });
    let wh_handle = ctx.systems.get_or_spawn(31_000_001);
    spawn_ship(&wh_handle, ship_id, 587, 25, 6, DVec3::ZERO, 1_000_000.0).await;
    sess.solar_system_id = Some(31_000_001);
    let wh_err = call_cyno(
        &ctx,
        &mut sess,
        "structureCynoBeaconMgr",
        "CmdJumpToStructureBeacon",
        &[PyRep::Integer(sys_id as i64), PyRep::Integer(beacon_id)],
    )
    .await
    .unwrap_err();
    assert_eq!(wh_err.code, "JumpDriveForbiddenInWormholeSpace");
    sess.solar_system_id = Some(sys_id);

    // 3. DirectionalScan & ConeScan: Zarzakh restriction, Combat Recon immunity (group 906), and Scan Inhibitor
    assert!(
        handle(&ctx, &mut sess, None, "scanMgr", "DirectionalScan", &[], &HashMap::new())
            .await
            .is_none()
    );
    sess.solar_system_id = Some(30_100_000);
    assert_eq!(
        call_cyno(&ctx, &mut sess, "scanMgr", "ConeScan", &[]).await.unwrap_err().code,
        "CannotDscanInThisSystem"
    );
    sess.solar_system_id = Some(sys_id);

    clone_type_with(&mut ctx.sde, 11_957, 906, 6, "Falcon", 10_000_000.0, &[]);
    let jita_handle = ctx.systems.get_or_spawn(sys_id);
    spawn_ship(&jita_handle, ship_id, 587, 25, 6, DVec3::ZERO, 1_000_000.0).await;
    let recon_id = 930_001_i64;
    let visible_id = 930_002_i64;
    spawn_ship(
        &jita_handle,
        recon_id,
        11_957,
        906,
        6,
        DVec3::new(10_000.0, 0.0, 0.0),
        10_000_000.0,
    )
    .await;
    spawn_ship(
        &jita_handle,
        visible_id,
        587,
        25,
        6,
        DVec3::new(10_000.0, 0.0, 0.0),
        1_000_000.0,
    )
    .await;

    let scan_res = call_cyno(
        &ctx,
        &mut sess,
        "scanMgr",
        "ConeScan",
        &[
            PyRep::Real(360.0),
            PyRep::Real(1_000_000.0),
            PyRep::Real(1.0),
            PyRep::Real(0.0),
            PyRep::Real(0.0),
        ],
    )
    .await
    .unwrap();
    let PyRep::List(rows) = scan_res else {
        panic!("expected List from ConeScan");
    };
    let hit_ids: Vec<i64> = rows
        .iter()
        .filter_map(|r| match r {
            PyRep::KeyVal(kv) => kv.get("id").and_then(PyRep::as_i64),
            _ => None,
        })
        .collect();
    assert!(hit_ids.contains(&visible_id));
    assert!(!hit_ids.contains(&recon_id));

    // Place an online Mobile Scan Inhibitor at (1000, 0, 0) -> caller at (0,0,0) is within 30 km, so ConeScan returns empty!
    let inhib_id = 930_003_i64;
    spawn_ship(
        &jita_handle,
        inhib_id,
        crate::services::mobile_deployables_ext::TYPE_MOBILE_SCAN_INHIBITOR,
        crate::services::mobile_deployables_ext::GROUP_MOBILE_SCAN_INHIBITOR,
        22,
        DVec3::new(1_000.0, 0.0, 0.0),
        1_000.0,
    )
    .await;
    let now_u = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        write_deployable_state_snap(snap, |st| {
            st.deployables.insert(
                inhib_id,
                MobileDeployableRecord {
                    item_id: inhib_id,
                    type_id: crate::services::mobile_deployables_ext::TYPE_MOBILE_SCAN_INHIBITOR,
                    group_id: crate::services::mobile_deployables_ext::GROUP_MOBILE_SCAN_INHIBITOR,
                    owner_id: char_id,
                    solar_system_id: sys_id,
                    anchor_start_unix: now_u.saturating_sub(120),
                    anchor_duration_secs: 60,
                    anchor_completed: true,
                    is_unanchored: false,
                    expires_at_unix: now_u + 3_600,
                    last_interaction_unix: now_u,
                    reinforced_until_unix: None,
                    last_ping_unix: None,
                    position: [1_000.0, 0.0, 0.0],
                    deployed_fleet_id: None,
                },
            );
        });
    });
    let balls = jita_handle.list_balls().await.unwrap_or_default();
    assert_eq!(active_scan_inhibitor_positions(&ctx, sys_id, &balls).len(), 1);
    let blocked_scan = call_cyno(
        &ctx,
        &mut sess,
        "scanMgr",
        "ConeScan",
        &[PyRep::Real(360.0), PyRep::Real(1_000_000.0)],
    )
    .await
    .unwrap();
    assert_eq!(blocked_scan, PyRep::List(vec![]));
}
