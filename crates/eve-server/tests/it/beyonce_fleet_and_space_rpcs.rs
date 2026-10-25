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

fn seed_in_space_ship(ctx: &ServerContext, char_id: i64, ship_id: i64, type_id: u32, sys_id: u32, pos: DVec3) {
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = sys_id;
            ch.active_ship_id = ship_id;
            ch.station_id = None;
            ch.structure_id = None;
        }
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id,
                owner_id: char_id,
                location_id: sys_id as i64,
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

    // 1. GetRelativity
    let rel = ctx
        .dispatch_rpc(&mut sess1, "beyonce", None, "GetRelativity", &[PyRep::Integer(1), PyRep::Integer(2), PyRep::Integer(3), PyRep::Integer(4), PyRep::Integer(5), PyRep::Integer(6)], &HashMap::new(), None)
        .await
        .expect("GetRelativity");
    let PyRep::Tuple(rel_tuple) = rel else { panic!("expected Tuple from GetRelativity, got {rel:?}") };
    assert_eq!(rel_tuple.len(), 7, "GetRelativity must return 7-tuple");

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

    // 5. CmdFleetTagTarget
    let tag_target_id = 99_000_123;
    let tag_res = ctx
        .dispatch_rpc(&mut sess1, "beyonce", None, "CmdFleetTagTarget", &[PyRep::Integer(tag_target_id), PyRep::String("1".into())], &HashMap::new(), None)
        .await
        .expect("CmdFleetTagTarget");
    assert_eq!(tag_res, PyRep::None);

    // 6. CmdFleetRegroup
    let regroup_res = ctx
        .dispatch_rpc(&mut sess1, "beyonce", None, "CmdFleetRegroup", &[], &HashMap::new(), None)
        .await
        .expect("CmdFleetRegroup");
    assert_eq!(regroup_res, PyRep::None);

    // 7. CmdGotoBookmark
    let bm_id = 70_000_001;
    ctx.db.with_mut_snapshot(|snap| {
        snap.bookmarks.insert(
            bm_id,
            BookmarkRecord {
                bookmark_id: bm_id,
                owner_id: C1,
                label: "Safe Spot".into(),
                solar_system_id: SYS_ORIGIN,
                x: 10_000.0,
                y: 20_000.0,
                z: 30_000.0,
                item_id: None,
                memo: "".into(),
                folder_id: None,
                is_shared: false,
            },
        );
    });
    let goto_bm_res = ctx
        .dispatch_rpc(&mut sess1, "beyonce", None, "CmdGotoBookmark", &[PyRep::Integer(bm_id)], &HashMap::new(), None)
        .await
        .expect("CmdGotoBookmark");
    assert_eq!(goto_bm_res, PyRep::None);
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

    // 1. Structure self destruct
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
            },
        );
    });

    let sd_start = ctx
        .dispatch_rpc(&mut sess, "beyonce", None, "CmdSelfDestructStructure", &[PyRep::Integer(struct_id)], &HashMap::new(), None)
        .await
        .expect("CmdSelfDestructStructure");
    assert_eq!(sd_start, PyRep::None);

    let sd_abort = ctx
        .dispatch_rpc(&mut sess, "beyonce", None, "CmdAbortSelfDestructStructure", &[PyRep::Integer(struct_id)], &HashMap::new(), None)
        .await
        .expect("CmdAbortSelfDestructStructure");
    assert_eq!(sd_abort, PyRep::None);

    // 2. GetOperationSpawnpointPosition
    let op_pos = ctx
        .dispatch_rpc(&mut sess, "beyonce", None, "GetOperationSpawnpointPosition", &[PyRep::Integer(struct_id)], &HashMap::new(), None)
        .await
        .expect("GetOperationSpawnpointPosition");
    let PyRep::Tuple(coords) = op_pos else { panic!("expected Tuple from GetOperationSpawnpointPosition") };
    assert_eq!(coords.len(), 3);
    assert_eq!(coords[0], PyRep::Real(1000.0));

    // 3. GetContainerExpiryDate
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

    // 4. GetCapacityOfSiphon
    let siphon_id = 77_000_001;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            siphon_id,
            InventoryItemRecord {
                item_id: siphon_id,
                type_id: 33475,
                owner_id: C1,
                location_id: SYS_ORIGIN as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            77_000_002,
            InventoryItemRecord {
                item_id: 77_000_002,
                type_id: 34, // Tritanium
                owner_id: C1,
                location_id: siphon_id,
                flag: InventoryFlag::None,
                quantity: 100,
                singleton: false,
                ..Default::default()
            },
        );
    });
    let cap_usage = ctx
        .dispatch_rpc(&mut sess, "beyonce", None, "GetCapacityOfSiphon", &[PyRep::Integer(siphon_id)], &HashMap::new(), None)
        .await
        .expect("GetCapacityOfSiphon");
    let PyRep::Real(pct) = cap_usage else { panic!("expected Real from GetCapacityOfSiphon") };
    assert!(pct >= 0.0 && pct <= 1.0, "Capacity fraction should be in [0, 1]");
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

    // Grant capital and jump drive skills to Pilot 1 (Avatar titan 11567)
    let titan_id = 81_000_001;
    let titan_type = 11567; // Avatar
    let portal_gen = 23953; // Jump Portal Generator I
    let portal_mod_id = 81_000_002;
    let fuel_type = 16275; // Strontium Clathrates
    let fuel_item_id = 81_000_003;

    ctx.db.with_mut_snapshot(|snap| {
        let entry1 = snap.skills.entry(C1).or_default();
        for sid in [20533, 21611, 21610, 24311, 24312] {
            entry1.insert(sid, eve_db::TrainedSkillRecord { type_id: sid, level: 5, skill_points: 256_000.0 });
        }
        let entry2 = snap.skills.entry(C2).or_default();
        entry2.insert(21603, eve_db::TrainedSkillRecord { type_id: 21603, level: 5, skill_points: 256_000.0 });
    });

    seed_in_space_ship(&ctx, C1, titan_id, titan_type, sys_origin, DVec3::ZERO);
    sess1.solar_system_id = Some(sys_origin);
    sess1.ship_id = Some(titan_id);

    // Fit portal generator and seed fuel in titan cargo
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            portal_mod_id,
            InventoryItemRecord {
                item_id: portal_mod_id,
                type_id: portal_gen,
                owner_id: C1,
                location_id: titan_id,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            fuel_item_id,
            InventoryItemRecord {
                item_id: fuel_item_id,
                type_id: fuel_type,
                owner_id: C1,
                location_id: titan_id,
                flag: InventoryFlag::Cargo,
                quantity: 50_000,
                singleton: false,
                ..Default::default()
            },
        );
        // Also add Helium Isotopes (fuel for jump drive)
        snap.items.insert(
            81_000_004,
            InventoryItemRecord {
                item_id: 81_000_004,
                type_id: 16274, // Helium Isotopes
                owner_id: C1,
                location_id: titan_id,
                flag: InventoryFlag::Cargo,
                quantity: 100_000,
                singleton: false,
                ..Default::default()
            },
        );
    });

    // Seed Deployable Cyno Beacon (type 56099) in space in sys_dest owned by Pilot 2
    let beacon_item_id = 77_000_003;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            beacon_item_id,
            InventoryItemRecord {
                item_id: beacon_item_id,
                type_id: 56099,
                owner_id: C2,
                location_id: sys_dest as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });

    // Form fleet with Pilot 1 & Pilot 2
    let fid = 601;
    seed_fleet(&ctx, fid, C1, vec![
        (C1, FleetRole::FleetCommander, None, None),
        (C2, FleetRole::SquadMember, Some(1), Some(1)),
    ]);
    sess1.fleet_id = Some(fid);
    sess2.fleet_id = Some(fid);

    // 1. Bridge to Deployable Beacon
    let bridge_res = ctx
        .dispatch_rpc(&mut sess1, "beyonce", None, "CmdBridgeToFleetDeployableBeacon", &[PyRep::Integer(C2), PyRep::Integer(sys_dest as i64), PyRep::Integer(beacon_item_id)], &HashMap::new(), None)
        .await
        .expect("CmdBridgeToFleetDeployableBeacon");
    assert_eq!(bridge_res.dict_get("shipID"), Some(&PyRep::Integer(titan_id)));

    // 2. Group Jump to Deployable Beacon
    let _group_jump_dep = ctx
        .dispatch_rpc(&mut sess1, "beyonce", None, "CmdGroupJumpToFleetDeployableBeacon", &[PyRep::Integer(C2), PyRep::Integer(sys_dest as i64), PyRep::Integer(beacon_item_id)], &HashMap::new(), None)
        .await
        .expect("CmdGroupJumpToFleetDeployableBeacon");
    assert_eq!(sess1.solar_system_id, Some(sys_dest));

    // Reset titan back to origin for module cyno test
    seed_in_space_ship(&ctx, C1, titan_id, titan_type, sys_origin, DVec3::ZERO);
    sess1.solar_system_id = Some(sys_origin);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&C1) {
            ch.jump_cooldown_until_unix = 0;
        }
    });

    // 3. Module cyno lit by Pilot 2 in sys_dest
    seed_in_space_ship(&ctx, C2, 82_000_001, 11957, sys_dest, DVec3::ZERO);
    sess2.solar_system_id = Some(sys_dest);
    sess2.ship_id = Some(82_000_001);

    // Light cyno module
    let cyno_mod_id = 82_000_002;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            cyno_mod_id,
            InventoryItemRecord {
                item_id: cyno_mod_id,
                type_id: 21096, // Cynosural Field Generator I
                owner_id: C2,
                location_id: 82_000_001,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            82_000_003,
            InventoryItemRecord {
                item_id: 82_000_003,
                type_id: 16273, // Liquid Ozone
                owner_id: C2,
                location_id: 82_000_001,
                flag: InventoryFlag::Cargo,
                quantity: 10_000,
                singleton: false,
                ..Default::default()
            },
        );
    });

    let _ = ctx
        .dispatch_rpc(&mut sess2, "dogmaIM", None, "Activate", &[PyRep::Integer(cyno_mod_id), PyRep::String("cynosuralGeneration".into()), PyRep::Integer(1)], &HashMap::new(), None)
        .await
        .expect("Activate cyno");

    // 4. Group Jump to Module Beacon
    let _group_jump_mod = ctx
        .dispatch_rpc(&mut sess1, "beyonce", None, "CmdGroupJumpToFleetModuleBeacon", &[PyRep::Integer(C2), PyRep::Integer(sys_dest as i64), PyRep::Integer(0)], &HashMap::new(), None)
        .await
        .expect("CmdGroupJumpToFleetModuleBeacon");
    assert_eq!(sess1.solar_system_id, Some(sys_dest));

    // Reset titan back to origin for jump to deployable beacon test
    seed_in_space_ship(&ctx, C1, titan_id, titan_type, sys_origin, DVec3::ZERO);
    sess1.solar_system_id = Some(sys_origin);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&C1) {
            ch.jump_cooldown_until_unix = 0;
        }
    });

    // 5. Jump to Deployable Beacon
    let _jump_dep = ctx
        .dispatch_rpc(&mut sess1, "beyonce", None, "CmdJumpToFleetDeployableBeacon", &[PyRep::Integer(C2), PyRep::Integer(sys_dest as i64), PyRep::Integer(beacon_item_id)], &HashMap::new(), None)
        .await
        .expect("CmdJumpToFleetDeployableBeacon");
    assert_eq!(sess1.solar_system_id, Some(sys_dest));
}
