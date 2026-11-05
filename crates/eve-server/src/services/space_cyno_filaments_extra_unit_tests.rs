//! Additional unit and mutation-killing tests for `space_cyno_filaments`.

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::mobile_deployables::{write_deployable_state_snap, MobileDeployableRecord};
use crate::services::space_destiny::space_cyno_filaments::{
    active_scan_inhibitor_positions, handle,
};
use eve_db::types::{FleetRecord, UpwellStructureRecord};
use eve_db::{InventoryFlag, InventoryItemRecord};
use eve_destiny::{Ball, CombatState, MovementMode, SlimItemMeta};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_sde::{InvType, SdeRegistry};
use glam::DVec3;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

static SHARED_SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();

fn test_ctx() -> ServerContext {
    if let Some(sde) = SHARED_SDE.get() {
        ServerContext::with_sde_and_repository(
            Arc::clone(sde),
            Arc::new(eve_db::WorldRepository::with_demo_seed()),
        )
        .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED)
    } else {
        let ctx = ServerContext::new();
        let _ = SHARED_SDE.set(Arc::clone(&ctx.sde));
        ctx
    }
}

fn clone_type_with(
    sde: &mut Arc<SdeRegistry>,
    new_tid: u32,
    group_id: u32,
    category_id: u32,
    name: &str,
    mass: f64,
    attrs: &[(u16, f64)],
) {
    let reg = Arc::make_mut(sde);
    let mut rec: InvType = reg.types.values().next().cloned().expect("base sde type");
    rec.type_id = new_tid;
    rec.group_id = group_id;
    rec.category_id = category_id;
    rec.name = name.to_string();
    rec.mass = mass;
    rec.default_attributes.clear();
    for &(k, v) in attrs {
        rec.default_attributes.insert(k, v);
    }
    reg.types.insert(new_tid, rec);
}

async fn spawn_ship(
    sys_h: &crate::system_actor::SystemHandle,
    id: i64,
    type_id: u32,
    group_id: u32,
    category_id: u32,
    position: DVec3,
    mass: f64,
) {
    let ball = Ball {
        id,
        type_id,
        position,
        velocity: DVec3::ZERO,
        max_velocity: 350.0,
        inertia: 2.0,
        mass,
        radius: 50.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    let meta = SlimItemMeta {
        owner_id: 90_000_001,
        char_id: Some(90_000_001),
        corp_id: 98_000_001,
        alliance_id: -1,
        group_id,
        category_id,
        security_status: 0.0,
        is_cloaked: false,
        ..Default::default()
    };
    let _ = sys_h
        .spawn_ball_with_meta(ball, CombatState::default(), meta)
        .await;
}

async fn call_cyno(
    ctx: &ServerContext,
    sess: &mut SessionState,
    svc: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let kw = HashMap::new();
    handle(ctx, sess, None, svc, method, args, &kw).await.unwrap()
}

#[tokio::test]
async fn wormhole_jump_validation_capital_restriction_polarization_mass_and_k162_transit() {
    let mut ctx = test_ctx();
    clone_type_with(&mut ctx.sde, 990_001, 659, 6, "Test Supercarrier", 20_000_000.0, &[]);
    let sys_id = 30_000_142_u32;
    let char_id = 90_000_001_i64;
    let ship_id = 100_000_001_i64;
    let mut sess = SessionState::new(801);
    sess.user_id = Some(1);
    sess.character_id = Some(char_id);
    sess.corporation_id = Some(98_000_001);
    sess.solar_system_id = Some(sys_id);
    sess.ship_id = Some(ship_id);

    // 1. ShipDocked, NoSolarSystem, ShipNotInSpace
    sess.station_id = Some(60_003_760);
    assert_eq!(
        call_cyno(&ctx, &mut sess, "wormholeMgr", "WormholeJump", &[]).await.unwrap_err().code,
        "ShipDocked"
    );
    sess.station_id = None;
    sess.solar_system_id = None;
    assert_eq!(
        call_cyno(&ctx, &mut sess, "wormholeMgr", "WormholeJump", &[]).await.unwrap_err().code,
        "NoSolarSystem"
    );
    sess.solar_system_id = Some(sys_id);
    assert_eq!(
        call_cyno(&ctx, &mut sess, "wormholeMgr", "WormholeJump", &[PyRep::Integer(750_000_001)])
            .await
            .unwrap_err()
            .code,
        "ShipNotInSpace"
    );

    // 2. Supercarrier restricted from wormhole -> ShipRestrictedFromWormhole
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let k162_id = 750_000_001_i64;
    spawn_ship(&sys_handle, k162_id, 26_272, 988, 2, DVec3::new(500.0, 0.0, 0.0), 1.0).await;
    ctx.db.with_mut_snapshot(|s| {
        if let Some(sh) = s.items.get_mut(&ship_id) {
            sh.type_id = 990_001;
        }
    });
    spawn_ship(&sys_handle, ship_id, 990_001, 659, 6, DVec3::ZERO, 20_000_000.0).await;
    assert_eq!(
        call_cyno(&ctx, &mut sess, "wormholeMgr", "WormholeJump", &[PyRep::Integer(k162_id)])
            .await
            .unwrap_err()
            .code,
        "ShipRestrictedFromWormhole"
    );

    // Replace with subcap ship (587)
    ctx.db.with_mut_snapshot(|s| {
        if let Some(sh) = s.items.get_mut(&ship_id) {
            sh.type_id = 587;
        }
    });
    let _ = sys_handle.remove_ball(ship_id).await;
    spawn_ship(&sys_handle, ship_id, 587, 25, 6, DVec3::ZERO, 1_200_000.0).await;

    // 3. MissingWormholeBallId, WormholeNotPresent, OutsideWormholeJumpRange, NoWormholeDestination
    assert_eq!(
        call_cyno(&ctx, &mut sess, "wormholeMgr", "WormholeJump", &[]).await.unwrap_err().code,
        "MissingWormholeBallId"
    );
    assert_eq!(
        call_cyno(&ctx, &mut sess, "wormholeMgr", "WormholeJump", &[PyRep::Integer(750_999_999)])
            .await
            .unwrap_err()
            .code,
        "WormholeNotPresent"
    );

    let wh_no_dest = 750_000_099_i64;
    spawn_ship(&sys_handle, wh_no_dest, 26_272, 988, 2, DVec3::new(5_000.0, 0.0, 0.0), 1.0).await;
    assert_eq!(
        call_cyno(&ctx, &mut sess, "wormholeMgr", "WormholeJump", &[PyRep::Integer(wh_no_dest)])
            .await
            .unwrap_err()
            .code,
        "OutsideWormholeJumpRange"
    );
    let _ = sys_handle.set_ball_position(wh_no_dest, DVec3::new(500.0, 0.0, 0.0)).await;
    assert_eq!(
        call_cyno(&ctx, &mut sess, "wormholeMgr", "WormholeJump", &[PyRep::Integer(wh_no_dest)])
            .await
            .unwrap_err()
            .code,
        "NoWormholeDestination"
    );

    // 4. Configure wormhole destination 31_000_001 and execute jump through canonical K162 (750_000_001) at 500 m
    ctx.db.with_mut_snapshot(|s| {
        let st = s.system_states.entry(sys_id).or_default();
        st.wormhole_destination_system_id = Some(31_000_001);
        st.wormhole_remaining_mass_kg = 1_000_000_000.0;
    });
    let jump_res = call_cyno(&ctx, &mut sess, "wormholeMgr", "WormholeJump", &[PyRep::Integer(k162_id)])
        .await
        .unwrap();
    let PyRep::KeyVal(jkv) = jump_res else {
        panic!("expected KeyVal from WormholeJump");
    };
    assert_eq!(jkv["solarSystemID"], PyRep::Integer(31_000_001));
    assert_eq!(sess.solar_system_id, Some(31_000_001));
}

#[tokio::test]
async fn random_jump_filament_pochven_restrictions_scramble_and_invulnerability() {
    let mut ctx = test_ctx();
    clone_type_with(
        &mut ctx.sde,
        56_065,
        4087,
        66,
        "Glorification-1 'Devana' Filament",
        1.0,
        &[(3025, 4087.0)],
    );
    clone_type_with(
        &mut ctx.sde,
        56_071,
        4087,
        66,
        "Border-5 'Pochven' Filament",
        1.0,
        &[(3025, 4087.0)],
    );
    let sys_id = 30_000_142_u32;
    let char_id = 90_000_001_i64;
    let ship_id = 100_000_001_i64;
    let mut sess = SessionState::new(802);
    sess.user_id = Some(1);
    sess.character_id = Some(char_id);
    sess.corporation_id = Some(98_000_001);
    sess.solar_system_id = Some(sys_id);
    sess.ship_id = Some(ship_id);
    sess.fleet_id = Some(501);

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let exit_fil_id = 920_001_i64;
    let entry_fil_id = 920_002_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.fleets.insert(
            501,
            FleetRecord {
                fleet_id: 501,
                boss_id: char_id,
                members: vec![char_id],
                free_move: true,
                ..Default::default()
            },
        );
        for (id, tid) in [(exit_fil_id, 56_065), (entry_fil_id, 56_071)] {
            snap.items.insert(
                id,
                InventoryItemRecord {
                    item_id: id,
                    type_id: tid,
                    owner_id: char_id,
                    location_id: ship_id,
                    flag: InventoryFlag::Cargo,
                    quantity: 1,
                    singleton: false,
                    version: 1,
                    ..Default::default()
                },
            );
        }
    });

    // 1. ShipInvulnerable when post-warp invulnerability timer is active
    let invuln_ball = Ball {
        id: ship_id,
        type_id: 587,
        position: DVec3::new(1.5e13, -2.1e13, 0.9e13),
        velocity: DVec3::ZERO,
        max_velocity: 350.0,
        inertia: 2.0,
        mass: 1_000_000.0,
        radius: 50.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    let _ = sys_handle
        .spawn_ball_with_combat(
            invuln_ball,
            CombatState {
                warp_exit_invulnerable_remaining_ticks: 10,
                ..Default::default()
            },
        )
        .await;
    assert_eq!(
        call_cyno(
            &ctx,
            &mut sess,
            "randomJumpMgr",
            "ActivateRandomJumpFilament",
            &[PyRep::Integer(exit_fil_id)]
        )
        .await
        .unwrap_err()
        .code,
        "ShipInvulnerable"
    );
    let _ = sys_handle.remove_ball(ship_id).await;
    spawn_ship(
        &sys_handle,
        ship_id,
        587,
        25,
        6,
        DVec3::new(1.5e13, -2.1e13, 0.9e13),
        1_000_000.0,
    )
    .await;

    // 2. Pochven exit filament used in Jita (non-Pochven) -> PochvenExitFilamentRequiresPochvenOrigin
    assert_eq!(
        call_cyno(
            &ctx,
            &mut sess,
            "randomJumpMgr",
            "ActivateRandomJumpFilament",
            &[PyRep::Integer(exit_fil_id)]
        )
        .await
        .unwrap_err()
        .code,
        "PochvenExitFilamentRequiresPochvenOrigin"
    );

    // 3. Pochven entry filament used from Wormhole space (31_000_001) -> PochvenEntryFilamentDisallowedOrigin
    let wh_sys = 31_000_001_u32;
    let wh_handle = ctx.systems.get_or_spawn(wh_sys);
    spawn_ship(
        &wh_handle,
        ship_id,
        587,
        25,
        6,
        DVec3::new(1.5e13, -2.1e13, 0.9e13),
        1_000_000.0,
    )
    .await;
    sess.solar_system_id = Some(wh_sys);
    assert_eq!(
        call_cyno(
            &ctx,
            &mut sess,
            "randomJumpMgr",
            "ActivateRandomJumpFilament",
            &[PyRep::Integer(entry_fil_id)]
        )
        .await
        .unwrap_err()
        .code,
        "PochvenEntryFilamentDisallowedOrigin"
    );
}

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
