//! Tests for 15 Beyonce space and fleet RPC handlers:
//! CmdFleetRegroup, CmdFleetTagTarget, CmdGotoBookmark, CmdSelfDestructStructure,
//! CmdAbortSelfDestructStructure, CmdJumpToFleetDeployableBeacon,
//! CmdBridgeToFleetDeployableBeacon, CmdGroupJumpToFleetDeployableBeacon,
//! CmdGroupJumpToFleetModuleBeacon, GetFleetLocationAndInfo, GetFleetState,
//! GetRelativity, GetContainerExpiryDate, GetCapacityOfSiphon,
//! GetOperationSpawnpointPosition.

use std::collections::{HashMap, HashSet};
use glam::DVec3;
use eve_db::{BookmarkRecord, FleetMemberRecord, FleetRecord, FleetRole, InventoryFlag, InventoryItemRecord, UpwellStructureRecord};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::services::combat_crimewatch::{with_mut_combat_runtime, ContainerLifetimeEntry};

use crate::common;

const C1: i64 = 90_000_001;
const C2: i64 = 90_000_002;
const SYS_ORIGIN: u32 = 30_000_142;
const SYS_DEST: u32 = 30_000_144;

fn new_ctx() -> ServerContext {
    let sde = std::sync::Arc::new(
        eve_sde::SdeRegistry::open(&eve_sde::SdeRegistry::default_db_path()).expect("open evomu_sde.sqlite"),
    );
    let db = std::sync::Arc::new(eve_db::WorldRepository::with_demo_seed());
    let ctx = ServerContext::with_sde_and_repository(sde, db);
    ctx.set_clock_paused(true);
    ctx
}

fn seed_item(snap: &mut eve_db::WorldStateSnapshot, item_id: i64, type_id: u32, owner_id: i64, location_id: i64, flag: InventoryFlag, quantity: i32) {
    snap.items.insert(
        item_id,
        InventoryItemRecord {
            item_id,
            type_id,
            owner_id,
            location_id,
            flag,
            quantity,
            singleton: quantity == 1,
            ..Default::default()
        },
    );
}

fn seed_skills(snap: &mut eve_db::WorldStateSnapshot, char_id: i64, skill_ids: &[u32]) {
    let entry = snap.skills.entry(char_id).or_default();
    for &sid in skill_ids {
        entry.insert(sid, eve_db::TrainedSkillRecord { type_id: sid, level: 5, skill_points: 256_000.0 });
    }
}

fn seed_in_space_ship(ctx: &ServerContext, char_id: i64, ship_id: i64, type_id: u32, sys_id: u32, pos: DVec3) {
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = sys_id;
            ch.active_ship_id = ship_id;
            ch.station_id = None;
            ch.structure_id = None;
        }
        seed_item(snap, ship_id, type_id, char_id, sys_id as i64, InventoryFlag::None, 1);
    });
    let handle = ctx.systems.get_or_spawn(sys_id);
    let ball = Ball {
        id: ship_id,
        type_id,
        position: pos,
        velocity: DVec3::ZERO,
        max_velocity: 200.0,
        inertia: 1.0,
        mass: 1000.0,
        radius: 50.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    handle.try_spawn_ball_with_meta(ball, ctx.build_ship_combat_state(char_id, ship_id, type_id), ctx.build_ship_slim_meta(char_id, type_id));
}

fn seed_fleet(ctx: &ServerContext, fleet_id: i64, boss_id: i64, members: Vec<(i64, FleetRole, Option<i64>, Option<i64>)>) {
    ctx.db.with_mut_snapshot(|snap| {
        let mut member_details = HashMap::new();
        let mut member_ids = Vec::new();
        for (cid, role, wid, sid) in &members {
            member_ids.push(*cid);
            member_details.insert(
                *cid,
                FleetMemberRecord {
                    character_id: *cid,
                    role: *role,
                    wing_id: *wid,
                    squad_id: *sid,
                    takes_fleet_warp: true,
                    accepts_regroup: true,
                    accepts_conduit_jumps: true,
                    job: 0,
                    role_name: format!("{role:?}"),
                    join_time_unix: 1,
                },
            );
        }
        snap.fleets.insert(
            fleet_id,
            FleetRecord {
                fleet_id,
                boss_id,
                members: member_ids,
                wings: vec![1],
                squads: vec![1],
                free_move: false,
                is_registered: false,
                wing_details: HashMap::new(),
                squad_details: HashMap::new(),
                member_details,
                invites: HashSet::new(),
                motd: "Fleet test".into(),
            },
        );
    });
}

async fn login_and_select(ctx: &ServerContext, sess: &mut SessionState, user: &str, pass: &str, char_id: i64) {
    common::login_pilot(ctx, sess, user, pass).await;
    ctx.dispatch_rpc(
        sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
        &HashMap::new(),
        None,
    )
    .await
    .expect("SelectCharacterID");
}

#[tokio::test]
async fn test_beyonce_fleet_management_and_space_rpcs() {
    // parity: rpc:beyonce.CmdFleetRegroup, rpc:beyonce.CmdFleetTagTarget, rpc:beyonce.GetFleetLocationAndInfo
    // parity: rpc:beyonce.GetFleetState, rpc:beyonce.CmdGotoBookmark, rpc:beyonce.GetRelativity
    eve_testkit::attest(&[
        "rpc:beyonce.CmdFleetRegroup",
        "rpc:beyonce.CmdFleetTagTarget",
        "rpc:beyonce.GetFleetLocationAndInfo",
        "rpc:beyonce.GetFleetState",
        "rpc:beyonce.CmdGotoBookmark",
        "rpc:beyonce.GetRelativity",
    ]);

    let ctx = new_ctx();
    let mut sess1 = SessionState::new(101);
    login_and_select(&ctx, &mut sess1, "demo", "demo-secret", C1).await;
    let mut sess2 = SessionState::new(102);
    login_and_select(&ctx, &mut sess2, "other", "other-secret", C2).await;

    // 1. GetRelativity (BF-05): real client clock sync
    let rel = ctx
        .dispatch_rpc(&mut sess1, "beyonce", None, "GetRelativity", &[PyRep::Integer(100), PyRep::Integer(200), PyRep::Integer(300), PyRep::Integer(400), PyRep::Integer(500), PyRep::Integer(1)], &HashMap::new(), None)
        .await
        .expect("GetRelativity");
    let PyRep::Tuple(rel_tuple) = rel else { panic!("expected Tuple from GetRelativity, got {rel:?}") };
    assert_eq!(rel_tuple.len(), 7, "GetRelativity must return 7-tuple");
    assert!(rel_tuple[0].as_i64().unwrap_or(0) > 0, "filetime tick should be positive");

    // 2. Setup ships in space and fleet
    let ship1 = 80_000_001;
    let ship2 = 80_000_002;
    seed_in_space_ship(&ctx, C1, ship1, 638, SYS_ORIGIN, DVec3::new(0.0, 0.0, 0.0));
    seed_in_space_ship(&ctx, C2, ship2, 638, SYS_ORIGIN, DVec3::new(5_000.0, 0.0, 0.0));
    sess1.solar_system_id = Some(SYS_ORIGIN);
    sess1.ship_id = Some(ship1);
    sess2.solar_system_id = Some(SYS_ORIGIN);
    sess2.ship_id = Some(ship2);

    let fid = 501;
    seed_fleet(&ctx, fid, C1, vec![
        (C1, FleetRole::FleetCommander, None, None),
        (C2, FleetRole::SquadMember, Some(1), Some(1)),
    ]);
    sess1.fleet_id = Some(fid);
    sess2.fleet_id = Some(fid);

    // 3. GetFleetState
    let state1 = ctx
        .dispatch_rpc(&mut sess1, "beyonce", None, "GetFleetState", &[], &HashMap::new(), None)
        .await
        .expect("GetFleetState");
    assert_eq!(state1.dict_get("fleetID"), Some(&PyRep::Integer(fid)));
    assert_eq!(state1.dict_get("isLeader"), Some(&PyRep::Bool(true)));

    let state2 = ctx
        .dispatch_rpc(&mut sess2, "beyonce", None, "GetFleetState", &[], &HashMap::new(), None)
        .await
        .expect("GetFleetState");
    assert_eq!(state2.dict_get("isLeader"), Some(&PyRep::Bool(false)));

    // 4. GetFleetLocationAndInfo
    let loc_info = ctx
        .dispatch_rpc(&mut sess1, "beyonce", None, "GetFleetLocationAndInfo", &[], &HashMap::new(), None)
        .await
        .expect("GetFleetLocationAndInfo");
    let PyRep::Dict(info_items) = loc_info else { panic!("expected Dict from GetFleetLocationAndInfo") };
    assert!(info_items.iter().any(|(k, _)| k == &PyRep::Integer(C1)));
    assert!(info_items.iter().any(|(k, _)| k == &PyRep::Integer(C2)));

    // 5. CmdFleetTagTarget (BF-03): non-leader rejection & leader tagging
    let tag_target_id = 99_000_123;
    let tag_err = ctx
        .dispatch_rpc(&mut sess2, "beyonce", None, "CmdFleetTagTarget", &[PyRep::Integer(tag_target_id), PyRep::String("1".into())], &HashMap::new(), None)
        .await;
    assert!(tag_err.is_err(), "Non-leader member cannot tag targets");

    let tag_res = ctx
        .dispatch_rpc(&mut sess1, "beyonce", None, "CmdFleetTagTarget", &[PyRep::Integer(tag_target_id), PyRep::String("1".into())], &HashMap::new(), None)
        .await
        .expect("CmdFleetTagTarget");
    assert_eq!(tag_res, PyRep::None);
    let init_st = ctx
        .dispatch_rpc(&mut sess1, "fleetMgr", None, "GetInitState", &[], &HashMap::new(), None)
        .await
        .expect("GetInitState after tag");
    let PyRep::Dict(tags) = init_st.dict_get("targetTags").expect("targetTags dict") else { panic!("targetTags not dict") };
    assert!(tags.iter().any(|(k, v)| k == &PyRep::Integer(tag_target_id) && v == &PyRep::String("1".into())));

    // 6. CmdFleetRegroup (BF-01): role check and Follow mode
    let regroup_err = ctx
        .dispatch_rpc(&mut sess2, "beyonce", None, "CmdFleetRegroup", &[], &HashMap::new(), None)
        .await;
    assert!(regroup_err.is_err(), "Squad member cannot order fleet regroup");

    let regroup_res = ctx
        .dispatch_rpc(&mut sess1, "beyonce", None, "CmdFleetRegroup", &[], &HashMap::new(), None)
        .await
        .expect("CmdFleetRegroup");
    assert_eq!(regroup_res, PyRep::None);

    let handle = ctx.systems.get_or_spawn(SYS_ORIGIN);
    let bp = handle.ballpark.read().unwrap();
    let b2 = bp.balls.get(&ship2).expect("ship2 ball");
    match b2.mode {
        MovementMode::Follow { target_id, range } => {
            assert_eq!(target_id, ship1);
            assert_eq!(range, 1_000.0);
        }
        ref other => panic!("expected Follow mode, got {other:?}"),
    }
    drop(bp);

    // 7. CmdGotoBookmark (BF-06): ownership check and warp vs subwarp
    let bm_far_id = 70_000_001;
    let bm_near_id = 70_000_002;
    ctx.db.with_mut_snapshot(|snap| {
        snap.bookmarks.insert(
            bm_far_id,
            BookmarkRecord {
                bookmark_id: bm_far_id,
                owner_id: C1,
                label: "Far Safe Spot".into(),
                solar_system_id: SYS_ORIGIN,
                x: 200_000_000.0, // >= 150 km
                y: 0.0,
                z: 0.0,
                item_id: None,
                memo: "".into(),
                folder_id: None,
                is_shared: false,
            },
        );
        snap.bookmarks.insert(
            bm_near_id,
            BookmarkRecord {
                bookmark_id: bm_near_id,
                owner_id: C1,
                label: "Near Spot".into(),
                solar_system_id: SYS_ORIGIN,
                x: 50_000.0, // < 150 km
                y: 0.0,
                z: 0.0,
                item_id: None,
                memo: "".into(),
                folder_id: None,
                is_shared: false,
            },
        );
    });

    // Unauthorized bookmark access by sess2
    let bm_err = ctx
        .dispatch_rpc(&mut sess2, "beyonce", None, "CmdGotoBookmark", &[PyRep::Integer(bm_far_id)], &HashMap::new(), None)
        .await;
    assert!(bm_err.is_err(), "unauthorized pilot cannot warp to private bookmark");

    // Warp mode for far bookmark
    ctx.dispatch_rpc(&mut sess1, "beyonce", None, "CmdGotoBookmark", &[PyRep::Integer(bm_far_id)], &HashMap::new(), None)
        .await
        .expect("CmdGotoBookmark far");
    let bp = handle.ballpark.read().unwrap();
    let b1 = bp.balls.get(&ship1).expect("ship1 ball");
    assert!(matches!(b1.mode, MovementMode::Warp { .. }), "Expected warp mode for >= 150km bookmark");
    drop(bp);

    // Sub-warp for near bookmark (sets GotoDirection)
    ctx.dispatch_rpc(&mut sess1, "beyonce", None, "CmdGotoBookmark", &[PyRep::Integer(bm_near_id)], &HashMap::new(), None)
        .await
        .expect("CmdGotoBookmark near");
    let bp = handle.ballpark.read().unwrap();
    let b1 = bp.balls.get(&ship1).expect("ship1 ball");
    assert!(matches!(b1.mode, MovementMode::GotoDirection { .. }), "Expected GotoDirection mode for < 150km bookmark");
}

#[tokio::test]
async fn test_beyonce_structures_containers_and_siphons() {
    // parity: rpc:beyonce.CmdSelfDestructStructure, rpc:beyonce.CmdAbortSelfDestructStructure
    // parity: rpc:beyonce.GetContainerExpiryDate, rpc:beyonce.GetCapacityOfSiphon, rpc:beyonce.GetOperationSpawnpointPosition
    eve_testkit::attest(&[
        "rpc:beyonce.CmdSelfDestructStructure",
        "rpc:beyonce.CmdAbortSelfDestructStructure",
        "rpc:beyonce.GetContainerExpiryDate",
        "rpc:beyonce.GetCapacityOfSiphon",
        "rpc:beyonce.GetOperationSpawnpointPosition",
    ]);

    let ctx = new_ctx();
    let mut sess = SessionState::new(101);
    login_and_select(&ctx, &mut sess, "demo", "demo-secret", C1).await;
    sess.solar_system_id = Some(SYS_ORIGIN);
    sess.corporation_id = Some(98_000_001);
    sess.corp_role = eve_db::corp_roles::ROLE_DIRECTOR;

    // 1. Structure self destruct (BF-02): persistence & role enforcement
    let struct_id = 95_000_001;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&C1) {
            ch.corporation_id = 98_000_001;
        }
        snap.structures.insert(
            struct_id,
            UpwellStructureRecord {
                structure_id: struct_id,
                type_id: 35832, // Astrahus
                owner_corp_id: 98_000_001,
                solar_system_id: SYS_ORIGIN,
                name: "Test Citadel".into(),
                position: [1000.0, 2000.0, 3000.0],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 100,
                shield_hp: 10000.0,
                armor_hp: 10000.0,
                hull_hp: 10000.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: None,
            },
        );
    });

    // Rejection without director / station manager / CEO role
    let mut sess_no_role = SessionState::new(102);
    login_and_select(&ctx, &mut sess_no_role, "other", "other-secret", C2).await;
    sess_no_role.solar_system_id = Some(SYS_ORIGIN);
    sess_no_role.corporation_id = Some(98_000_001);
    sess_no_role.corp_role = 0;
    let sd_unauth = ctx
        .dispatch_rpc(&mut sess_no_role, "beyonce", None, "CmdSelfDestructStructure", &[PyRep::Integer(struct_id)], &HashMap::new(), None)
        .await;
    assert!(sd_unauth.is_err(), "Caller without director role cannot self-destruct structure");

    let sd_start = ctx
        .dispatch_rpc(&mut sess, "beyonce", None, "CmdSelfDestructStructure", &[PyRep::Integer(struct_id)], &HashMap::new(), None)
        .await
        .expect("CmdSelfDestructStructure");
    assert_eq!(sd_start, PyRep::None);
    let sd_val = ctx.db.with_snapshot(|snap| snap.structures.get(&struct_id).unwrap().self_destruct_at_unix);
    assert!(sd_val.is_some(), "self_destruct_at_unix must be set after CmdSelfDestructStructure");

    let sd_abort = ctx
        .dispatch_rpc(&mut sess, "beyonce", None, "CmdAbortSelfDestructStructure", &[PyRep::Integer(struct_id)], &HashMap::new(), None)
        .await
        .expect("CmdAbortSelfDestructStructure");
    assert_eq!(sd_abort, PyRep::None);
    let sd_val_after = ctx.db.with_snapshot(|snap| snap.structures.get(&struct_id).unwrap().self_destruct_at_unix);
    assert!(sd_val_after.is_none(), "self_destruct_at_unix must be None after CmdAbortSelfDestructStructure");

    // 2. GetOperationSpawnpointPosition (BF-06): SiteNotFound error
    let op_err = ctx
        .dispatch_rpc(&mut sess, "beyonce", None, "GetOperationSpawnpointPosition", &[PyRep::Integer(999_999_999)], &HashMap::new(), None)
        .await;
    assert!(op_err.is_err(), "missing site must return error");

    let op_pos = ctx
        .dispatch_rpc(&mut sess, "beyonce", None, "GetOperationSpawnpointPosition", &[PyRep::Integer(struct_id)], &HashMap::new(), None)
        .await
        .expect("GetOperationSpawnpointPosition");
    let PyRep::Tuple(coords) = op_pos else { panic!("expected Tuple from GetOperationSpawnpointPosition") };
    assert_eq!(coords.len(), 3);
    assert_eq!(coords[0], PyRep::Real(1000.0));

    // 3. GetContainerExpiryDate (BF-06): ContainerNotFound error
    let can_err = ctx
        .dispatch_rpc(&mut sess, "beyonce", None, "GetContainerExpiryDate", &[PyRep::Integer(999_999_999)], &HashMap::new(), None)
        .await;
    assert!(can_err.is_err(), "missing container must return error");

    let can_id = 88_000_001;
    with_mut_combat_runtime(&ctx, |rt| {
        rt.container_lifetimes.insert(
            can_id,
            ContainerLifetimeEntry {
                item_id: can_id,
                solar_system_id: SYS_ORIGIN,
                type_id: 23,
                owner_id: C1,
                created_unix: 1_000,
                lifetime_secs: 7_200,
                is_abandoned: false,
            },
        );
    });
    let expiry = ctx
        .dispatch_rpc(&mut sess, "beyonce", None, "GetContainerExpiryDate", &[PyRep::Integer(can_id)], &HashMap::new(), None)
        .await
        .expect("GetContainerExpiryDate");
    let PyRep::Integer(ft) = expiry else { panic!("expected Integer from GetContainerExpiryDate") };
    assert!(ft > 0, "Expiry filetime should be positive");

    // 4. GetCapacityOfSiphon (BF-06): ItemNotFound error
    let siphon_err = ctx
        .dispatch_rpc(&mut sess, "beyonce", None, "GetCapacityOfSiphon", &[PyRep::Integer(999_999_999)], &HashMap::new(), None)
        .await;
    assert!(siphon_err.is_err(), "missing siphon must return error");

    let siphon_id = 77_000_001;
    ctx.db.with_mut_snapshot(|snap| {
        seed_item(snap, siphon_id, 33475, C1, SYS_ORIGIN as i64, InventoryFlag::None, 1);
        seed_item(snap, 77_000_002, 34, C1, siphon_id, InventoryFlag::None, 100);
    });
    let cap_usage = ctx
        .dispatch_rpc(&mut sess, "beyonce", None, "GetCapacityOfSiphon", &[PyRep::Integer(siphon_id)], &HashMap::new(), None)
        .await
        .expect("GetCapacityOfSiphon");
    let PyRep::Real(pct) = cap_usage else { panic!("expected Real from GetCapacityOfSiphon") };
    assert!((0.0..=1.0).contains(&pct), "Capacity fraction should be in [0, 1]");
}

#[tokio::test]
async fn test_beyonce_cyno_deployable_and_group_jumps() {
    // parity: rpc:beyonce.CmdJumpToFleetDeployableBeacon, rpc:beyonce.CmdBridgeToFleetDeployableBeacon
    // parity: rpc:beyonce.CmdGroupJumpToFleetDeployableBeacon, rpc:beyonce.CmdGroupJumpToFleetModuleBeacon
    eve_testkit::attest(&[
        "rpc:beyonce.CmdJumpToFleetDeployableBeacon",
        "rpc:beyonce.CmdBridgeToFleetDeployableBeacon",
        "rpc:beyonce.CmdGroupJumpToFleetDeployableBeacon",
        "rpc:beyonce.CmdGroupJumpToFleetModuleBeacon",
    ]);

    let ctx = new_ctx();
    let mut sess1 = SessionState::new(101);
    login_and_select(&ctx, &mut sess1, "demo", "demo-secret", C1).await;
    let mut sess2 = SessionState::new(102);
    login_and_select(&ctx, &mut sess2, "other", "other-secret", C2).await;

    let (sys_origin, sys_dest) = {
        let sys: Vec<u32> = ctx
            .sde
            .solar_systems
            .values()
            .filter(|s| s.security_status < 0.45 && s.solar_system_id < 31_000_000 && s.galactic_pos_ly != [0.0; 3])
            .map(|s| s.solar_system_id)
            .collect();
        let mut pair = (SYS_ORIGIN, SYS_DEST);
        'outer: for &a in &sys {
            let pa = ctx.sde.solar_systems[&a].galactic_pos_ly;
            for &b in &sys {
                if a != b {
                    let pb = ctx.sde.solar_systems[&b].galactic_pos_ly;
                    let d = ((pa[0] - pb[0]).powi(2) + (pa[1] - pb[1]).powi(2) + (pa[2] - pb[2]).powi(2)).sqrt();
                    if (0.3..3.0).contains(&d) {
                        pair = (a, b);
                        break 'outer;
                    }
                }
            }
        }
        pair
    };

    ctx.db.with_mut_snapshot(|snap| {
        seed_skills(snap, C1, &[20533, 21611, 21610, 24311, 24312]);
        seed_skills(snap, C2, &[21603]);
    });

    let titan_id = 81_000_001;
    let titan_type = 11567; // Avatar
    let portal_gen = 23953; // Jump Portal Generator I
    let portal_mod_id = 81_000_002;
    seed_in_space_ship(&ctx, C1, titan_id, titan_type, sys_origin, DVec3::ZERO);
    sess1.solar_system_id = Some(sys_origin);
    sess1.ship_id = Some(titan_id);

    ctx.db.with_mut_snapshot(|snap| {
        seed_item(snap, portal_mod_id, portal_gen, C1, titan_id, InventoryFlag::HiSlot0, 1);
        seed_item(snap, 81_000_003, 16275, C1, titan_id, InventoryFlag::Cargo, 50_000); // Strontium
        seed_item(snap, 81_000_004, 16274, C1, titan_id, InventoryFlag::Cargo, 100_000); // Helium Isotopes
    });

    // Deployable Cyno Beacon (56099) anchored in sys_dest
    let beacon_item_id = 77_000_003;
    let beacon_owner = 99_999;
    ctx.db.with_mut_snapshot(|snap| {
        seed_item(snap, beacon_item_id, 56099, beacon_owner, sys_dest as i64, InventoryFlag::None, 1);
    });

    let fid = 601;
    seed_fleet(&ctx, fid, C1, vec![
        (C1, FleetRole::FleetCommander, None, None),
        (C2, FleetRole::SquadMember, Some(1), Some(1)),
    ]);
    sess1.fleet_id = Some(fid);
    sess2.fleet_id = Some(fid);

    // Pilot 2 in sys_origin within 10km of Titan
    let ship2_id = 82_000_001;
    seed_in_space_ship(&ctx, C2, ship2_id, 624, sys_origin, DVec3::new(1000.0, 0.0, 0.0));
    sess2.solar_system_id = Some(sys_origin);
    sess2.ship_id = Some(ship2_id);

    // 1. Bridge to Deployable Beacon
    let bridge_res = ctx
        .dispatch_rpc(&mut sess1, "beyonce", None, "CmdBridgeToFleetDeployableBeacon", &[PyRep::Integer(beacon_owner), PyRep::Integer(sys_dest as i64), PyRep::Integer(beacon_item_id)], &HashMap::new(), None)
        .await
        .expect("CmdBridgeToFleetDeployableBeacon");
    assert_eq!(bridge_res.dict_get("shipID"), Some(&PyRep::Integer(titan_id)));

    // 2. Jump through fleet deployable bridge (Pilot 2 jumps through Titan's bridge)
    let jump_thru = ctx
        .dispatch_rpc(&mut sess2, "beyonce", None, "CmdJumpThroughFleet", &[PyRep::Integer(C1), PyRep::Integer(titan_id)], &HashMap::new(), None)
        .await
        .expect("CmdJumpThroughFleet");
    assert_eq!(jump_thru.dict_get("solarSystemID"), Some(&PyRep::Integer(sys_dest as i64)));
    assert_eq!(sess2.solar_system_id, Some(sys_dest));

    // 3. Titan conduit jump rejection (BF-04: TitanCannotConduitJump)
    let titan_conduit_dep = ctx
        .dispatch_rpc(&mut sess1, "beyonce", None, "CmdGroupJumpToFleetDeployableBeacon", &[PyRep::Integer(beacon_owner), PyRep::Integer(sys_dest as i64), PyRep::Integer(beacon_item_id)], &HashMap::new(), None)
        .await;
    assert!(titan_conduit_dep.is_err(), "Titan cannot perform conduit jumps");

    let titan_conduit_mod = ctx
        .dispatch_rpc(&mut sess1, "beyonce", None, "CmdGroupJumpToFleetModuleBeacon", &[PyRep::Integer(beacon_owner), PyRep::Integer(sys_dest as i64), PyRep::Integer(0)], &HashMap::new(), None)
        .await;
    assert!(titan_conduit_mod.is_err(), "Titan cannot perform conduit jumps");

    // 4. Titan solo jump to deployable beacon
    let jump_dep = ctx
        .dispatch_rpc(&mut sess1, "beyonce", None, "CmdJumpToFleetDeployableBeacon", &[PyRep::Integer(beacon_owner), PyRep::Integer(sys_dest as i64), PyRep::Integer(beacon_item_id)], &HashMap::new(), None)
        .await
        .expect("CmdJumpToFleetDeployableBeacon");
    assert_eq!(jump_dep.dict_get("solarSystemID"), Some(&PyRep::Integer(sys_dest as i64)));
    assert_eq!(sess1.solar_system_id, Some(sys_dest));

    // 5. Carrier conduit jump to deployable beacon with passenger (BF-04)
    let carrier_id = 83_000_001;
    let carrier_type = 23757; // Archon
    seed_in_space_ship(&ctx, C1, carrier_id, carrier_type, sys_origin, DVec3::ZERO);
    seed_in_space_ship(&ctx, C2, ship2_id, 624, sys_origin, DVec3::new(2000.0, 0.0, 0.0));
    sess1.solar_system_id = Some(sys_origin);
    sess1.ship_id = Some(carrier_id);
    sess2.solar_system_id = Some(sys_origin);
    sess2.ship_id = Some(ship2_id);

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&C1) {
            ch.jump_cooldown_until_unix = 0;
            ch.solar_system_id = sys_origin;
        }
        if let Some(ch) = snap.characters.get_mut(&C2) {
            ch.jump_cooldown_until_unix = 0;
            ch.solar_system_id = sys_origin;
        }
        seed_item(snap, 83_000_002, 16274, C1, carrier_id, InventoryFlag::Cargo, 100_000);
    });

    let carrier_conduit = ctx
        .dispatch_rpc(&mut sess1, "beyonce", None, "CmdGroupJumpToFleetDeployableBeacon", &[PyRep::Integer(beacon_owner), PyRep::Integer(sys_dest as i64), PyRep::Integer(beacon_item_id)], &HashMap::new(), None)
        .await
        .expect("Carrier CmdGroupJumpToFleetDeployableBeacon");
    assert_eq!(carrier_conduit.dict_get("solarSystemID"), Some(&PyRep::Integer(sys_dest as i64)));
    assert_eq!(sess1.solar_system_id, Some(sys_dest));
    let p2_sys = ctx.db.with_snapshot(|snap| snap.characters.get(&C2).unwrap().solar_system_id);
    assert_eq!(p2_sys, sys_dest, "Passenger must be relocated to sys_dest by conduit jump");

    // 6. Carrier conduit jump to module cyno
    seed_in_space_ship(&ctx, C1, carrier_id, carrier_type, sys_origin, DVec3::ZERO);
    sess1.solar_system_id = Some(sys_origin);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&C1) {
            ch.jump_cooldown_until_unix = 0;
            ch.solar_system_id = sys_origin;
        }
        seed_item(snap, 83_000_003, 16274, C1, carrier_id, InventoryFlag::Cargo, 100_000);
    });

    // Pilot 2 lights cyno module in sys_dest
    seed_in_space_ship(&ctx, C2, ship2_id, 11957, sys_dest, DVec3::ZERO);
    sess2.solar_system_id = Some(sys_dest);
    let cyno_mod_id = 82_000_002;
    ctx.db.with_mut_snapshot(|snap| {
        seed_item(snap, cyno_mod_id, 21096, C2, ship2_id, InventoryFlag::HiSlot0, 1);
        seed_item(snap, 82_000_003, 16273, C2, ship2_id, InventoryFlag::Cargo, 10_000);
    });
    ctx.dispatch_rpc(&mut sess2, "dogmaIM", None, "Activate", &[PyRep::Integer(cyno_mod_id), PyRep::String("cynosuralGeneration".into()), PyRep::Integer(1)], &HashMap::new(), None)
        .await
        .expect("Activate cyno");

    let mod_conduit = ctx
        .dispatch_rpc(&mut sess1, "beyonce", None, "CmdGroupJumpToFleetModuleBeacon", &[PyRep::Integer(C2), PyRep::Integer(sys_dest as i64), PyRep::Integer(0)], &HashMap::new(), None)
        .await
        .expect("Carrier CmdGroupJumpToFleetModuleBeacon");
    assert_eq!(mod_conduit.dict_get("solarSystemID"), Some(&PyRep::Integer(sys_dest as i64)));
    assert_eq!(sess1.solar_system_id, Some(sys_dest));
}
