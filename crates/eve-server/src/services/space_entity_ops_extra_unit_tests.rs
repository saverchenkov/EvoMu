//! Additional unit and mutation-killing tests for `space_entity_ops`.

#![forbid(unsafe_code)]

use crate::router::{BoundObjectEntry, ServerContext};
use crate::services::combat_crimewatch::{
    with_mut_combat_runtime, ContainerLifetimeEntry, DroneSpaceState,
};
use crate::services::space_entity_ops::handle;
use eve_db::types::{FleetMemberRecord, FleetRecord, FleetRole};
use eve_db::{
    corp_roles, BookmarkRecord, CorporationRecord, InventoryFlag, InventoryItemRecord,
    UpwellStructureRecord,
};
use eve_destiny::{Ball, CombatState, MovementMode, TargetLock};
use eve_machonet::{service_roles, SessionState};
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

fn make_session(
    char_id: i64,
    corp_id: i64,
    sys_id: Option<u32>,
    ship_id: Option<i64>,
    role: u64,
) -> SessionState {
    let mut s = SessionState::new(601);
    s.user_id = Some(9);
    s.character_id = Some(char_id);
    s.corporation_id = Some(corp_id);
    s.solar_system_id = sys_id;
    s.ship_id = ship_id;
    s.role = role;
    s
}

async fn call_ops(
    ctx: &ServerContext,
    sess: &mut SessionState,
    svc: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let kw = HashMap::new();
    handle(ctx, sess, None, svc, method, args, &kw).await.unwrap()
}

fn make_ball(id: i64, type_id: u32, pos: DVec3) -> Ball {
    Ball {
        id,
        type_id,
        position: pos,
        velocity: DVec3::ZERO,
        max_velocity: 300.0,
        inertia: 1.0,
        mass: 1_000_000.0,
        radius: 40.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    }
}

#[tokio::test]
async fn entity_unanchor_behavior_debug_extra_fleet_members_and_shipping_unit_branches() {
    let mut ctx = test_ctx();
    let sys_id = 30_000_142_u32;
    let char_id = 90_000_001_i64;
    let other_char = 90_000_002_i64;
    let corp_id = 98_000_001_i64;
    let ship_id = 100_000_001_i64;
    let mut sess = make_session(char_id, corp_id, Some(sys_id), Some(ship_id), service_roles::ROLE_GM_DEFAULT);
    let kw = HashMap::new();

    {
        let reg = Arc::make_mut(&mut ctx.sde);
        let mut t: InvType = reg.types.values().next().cloned().unwrap();
        t.type_id = 33_474;
        t.group_id = 1246;
        t.category_id = 22;
        reg.types.insert(33_474, t);
    }

    // 1. CmdUnanchor with 2-arg ([ids], target_id) and structure owned by caller_corp_id
    let struct_id = 1_000_000_800_001_i64;
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let _ = sys_handle.spawn_ball(make_ball(ship_id, 587, DVec3::ZERO)).await;
    let _ = sys_handle
        .spawn_ball(make_ball(struct_id, 35832, DVec3::new(1_000.0, 0.0, 0.0)))
        .await;
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            struct_id,
            UpwellStructureRecord {
                structure_id: struct_id,
                solar_system_id: sys_id,
                owner_corp_id: corp_id,
                type_id: 35832,
                name: "Corp Struct".into(),
                state: "ShieldVulnerable".into(),
                ..Default::default()
            },
        );
    });
    let res = call_ops(
        &ctx,
        &mut sess,
        "entity",
        "CmdUnanchor",
        &[PyRep::List(vec![]), PyRep::Integer(struct_id)],
    )
    .await
    .unwrap();
    assert!(matches!(res, PyRep::KeyVal(_)));

    // 2. EnableBehaviorDebugging / DisableBehaviorDebugging via kwargs + bound_entry fallback
    let mut b_kw = HashMap::new();
    b_kw.insert("entityID".into(), PyRep::Integer(555_001));
    assert!(handle(&ctx, &mut sess, None, "entity", "EnableBehaviorDebugging", &[], &b_kw)
        .await
        .unwrap()
        .is_ok());
    assert!(handle(&ctx, &mut sess, None, "entity", "DisableBehaviorDebugging", &[], &b_kw)
        .await
        .unwrap()
        .is_ok());
    let mut no_sys_sess = sess.clone();
    no_sys_sess.solar_system_id = None;
    let bound = BoundObjectEntry {
        bound_id: 1,
        session_id: sess.session_id,
        user_id: 9,
        character_id: char_id,
        service: "entity".into(),
        object_id: sys_id as i64,
        oid_str: format!("N=1:{sys_id}"),
        ref_id: 1,
    };
    assert!(handle(&ctx, &mut no_sys_sess, Some(&bound), "entity", "GetBehaviorTreeManager", &[], &kw)
        .await
        .unwrap()
        .is_ok());

    // 3. GetExtraFleetMembers with Guard role
    ctx.db.with_mut_snapshot(|snap| {
        snap.fleets.insert(
            801,
            FleetRecord {
                fleet_id: 801,
                boss_id: char_id,
                members: vec![char_id, other_char],
                free_move: true,
                ..Default::default()
            },
        );
    });
    with_mut_combat_runtime(&ctx, |rt| {
        rt.active_drones_in_space.insert(
            777_001,
            DroneSpaceState {
                ball_id: 777_001,
                drone_item_id: 777_001,
                type_id: 2456,
                group_id: 100,
                owner_char_id: other_char,
                original_owner_char_id: other_char,
                parent_ship_id: 200,
                solar_system_id: sys_id,
                target_id: None,
                activity_state: 1,
                assist_char_id: None,
                guard_char_id: Some(char_id),
                is_abandoned: false,
                is_sentry: false,
                bandwidth_used: 5.0,
                max_velocity: 400.0,
                optimal_range: 2000.0,
                damage_vector: Default::default(),
                shield_repair_amount: 0.0,
                armor_repair_amount: 0.0,
                hull_repair_amount: 0.0,
                web_speed_factor: 0.0,
                neut_amount: 0.0,
                ecm_strength: 0.0,
                tp_sig_bonus_pct: 0.0,
            },
        );
    });
    let extra = call_ops(&ctx, &mut sess, "entity", "GetExtraFleetMembers", &[]).await.unwrap();
    assert!(matches!(extra, PyRep::List(ref v) if v.len() == 1));

    // 4. OpenShippingUnitStorage errors & personal deployable restriction against fleetmate
    assert_eq!(
        call_ops(&ctx, &mut sess, "entity", "OpenShippingUnitStorage", &[])
            .await
            .unwrap_err()
            .code,
        "InvalidArgument"
    );
    assert_eq!(
        call_ops(&ctx, &mut sess, "entity", "OpenShippingUnitStorage", &[PyRep::Integer(999_888)])
            .await
            .unwrap_err()
            .code,
        "UnitNotInSpace"
    );

    let dep_id = 777_002_i64;
    let _ = sys_handle
        .spawn_ball(make_ball(dep_id, 33_474, DVec3::new(1_000.0, 0.0, 0.0)))
        .await;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            dep_id,
            InventoryItemRecord {
                item_id: dep_id,
                type_id: 33_474,
                owner_id: other_char,
                location_id: sys_id as i64,
                flag: InventoryFlag::Unlocked,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });
    let mut non_gm = sess.clone();
    non_gm.role = service_roles::ROLE_PLAYER_DEFAULT;
    assert_eq!(
        call_ops(&ctx, &mut non_gm, "entity", "OpenShippingUnitStorage", &[PyRep::Integer(dep_id)])
            .await
            .unwrap_err()
            .code,
        "AccessDenied"
    );
}

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
            CorporationRecord {
                corporation_id: target_corp,
                name: "Target Corp".into(),
                ticker: "TGT".into(),
                ceo_id: 90_000_002,
                ..Default::default()
            },
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
