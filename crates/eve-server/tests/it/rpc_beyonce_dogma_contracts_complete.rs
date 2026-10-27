//! Integration coverage for the 75 implemented `beyonce`, `dogmaIM`, `contractProxy`, and
//! economy/service RPC handlers against real V24.01 SDE data and world state.

#![forbid(unsafe_code)]

use crate::common;
use eve_db::{InventoryFlag, InventoryItemRecord, TrainedSkillRecord};
use eve_destiny::{Ball, DVec3, MovementMode};
use eve_machonet::{service_roles, SessionState};
use eve_proto::PyRep;
use eve_sde::attr_ids;
use eve_server::router::ServerContext;
use std::collections::HashMap;
use std::sync::Arc;

const C1: i64 = 90_000_001;
const C2: i64 = 90_000_002;
const C3: i64 = 90_000_003;
const JITA: u32 = 30_000_142;
const JITA_44: i64 = 60_003_760;

fn new_ctx() -> ServerContext {
    let sde = Arc::new(eve_sde::SdeRegistry::open(&eve_sde::SdeRegistry::default_db_path()).expect("sde"));
    let ctx = ServerContext::with_sde_and_repository(sde, Arc::new(eve_db::WorldRepository::with_demo_seed()));
    ctx.set_clock_paused(true);
    ctx
}

async fn rpc(ctx: &ServerContext, sess: &mut SessionState, svc: &str, method: &str, args: &[PyRep]) -> PyRep {
    ctx.dispatch_rpc(sess, svc, None, method, args, &HashMap::new(), None)
        .await.unwrap_or_else(|e| panic!("{svc}.{method} failed: {e:?}"))
}

async fn rpc_bound(ctx: &ServerContext, sess: &mut SessionState, svc: &str, bid: u64, method: &str, args: &[PyRep]) -> PyRep {
    ctx.dispatch_rpc(sess, svc, Some(bid), method, args, &HashMap::new(), None)
        .await.unwrap_or_else(|e| panic!("{svc}.{method} (bound) failed: {e:?}"))
}

async fn login_select(ctx: &ServerContext, char_id: i64, sid: u64) -> SessionState {
    if char_id == C3 {
        ctx.db.with_mut_snapshot(|snap| {
            snap.accounts.insert("third".into(), eve_db::AccountRecord {
                user_id: 3, username: "third".into(), password_token: eve_db::WorldRepository::hash_password_token("third", "third-secret"),
                role: eve_db::service_roles::ROLE_PLAYER_DEFAULT, banned_until_unix: None, ban_reason: None,
            });
            let mut c = snap.characters[&C2].clone();
            c.character_id = C3; c.user_id = 3; c.name = "Third Pilot".into();
            snap.characters.insert(C3, c);
            let sk = snap.skills.get(&C2).cloned().unwrap_or_default();
            snap.skills.insert(C3, sk);
        });
    }
    let (user, pass) = match char_id { C1 => ("demo", "demo-secret"), C2 => ("other", "other-secret"), _ => ("third", "third-secret") };
    let mut sess = SessionState::new(sid);
    common::login_pilot(ctx, &mut sess, user, pass).await;
    rpc(ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(char_id)]).await;
    ctx.advance_secs(12); sess
}

fn seed_item(ctx: &ServerContext, item_id: i64, type_id: u32, owner: i64, loc: i64, flag: InventoryFlag, qty: i32) {
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(item_id, InventoryItemRecord {
            item_id, type_id, owner_id: owner, location_id: loc, flag, quantity: qty, singleton: qty == 1, version: 1,
            dynamic_attributes: HashMap::new(), source_mutaplasmid_id: None, insured_payout_isk: 0.0, is_blueprint_copy: false, blueprint_runs_remaining: None,
        });
    });
}

fn grant_req_skills(ctx: &ServerContext, char_id: i64, type_id: u32) {
    let pairs = [attr_ids::REQUIRED_SKILL_1, attr_ids::REQUIRED_SKILL_2, attr_ids::REQUIRED_SKILL_3];
    let skills: Vec<u32> = pairs
        .iter()
        .filter_map(|s| ctx.sde.types[&type_id].default_attributes.get(s).map(|&v| v as u32))
        .filter(|&s| s > 0)
        .collect();
    ctx.db.with_mut_snapshot(|snap| {
        let entry = snap.skills.entry(char_id).or_default();
        for sid in skills { entry.insert(sid, TrainedSkillRecord { type_id: sid, level: 5, skill_points: 256_000.0 }); }
    });
}

async fn board_in_space(ctx: &ServerContext, sess: &mut SessionState, char_id: i64, ship_id: i64, type_id: u32, sys_id: u32, pos: DVec3) {
    grant_req_skills(ctx, char_id, type_id);
    if ctx.db.with_snapshot(|s| !s.items.contains_key(&ship_id)) {
        seed_item(ctx, ship_id, type_id, char_id, sys_id as i64, InventoryFlag::None, 1);
    }
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(sh) = snap.items.get_mut(&ship_id) {
            sh.location_id = sys_id as i64;
            sh.flag = InventoryFlag::None;
        }
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.active_ship_id = ship_id;
            ch.station_id = None;
            ch.solar_system_id = sys_id;
            ch.jump_cooldown_until_unix = 0;
        }
    });
    sess.ship_id = Some(ship_id);
    sess.station_id = None; sess.structure_id = None; sess.solar_system_id = Some(sys_id);
    ctx.invalidate_ship_fitting(ship_id);
    let t = &ctx.sde.types[&type_id];
    let ball = Ball {
        id: ship_id, type_id, position: pos, velocity: DVec3::ZERO, max_velocity: 250.0,
        inertia: 1.0, mass: t.mass, radius: t.radius, speed_fraction: 0.0, mode: MovementMode::Stop,
    };
    let (cs, meta) = (ctx.build_ship_combat_state(char_id, ship_id, type_id), ctx.build_ship_slim_meta(char_id, type_id));
    ctx.systems.get_or_spawn(sys_id).spawn_ball_with_meta(ball, cs, meta).await.expect("spawn ball");
}

#[tokio::test]
async fn test_rpc_beyonce_movement_and_warp_commands() {
    // parity: rpc:beyonce.CmdSetSpeedFraction, rpc:beyonce.CmdGotoDirection, rpc:beyonce.CmdSteerDirection
    // parity: rpc:beyonce.CmdStop, rpc:beyonce.CmdAlignTo, rpc:beyonce.CmdOrbit
    // parity: rpc:beyonce.CmdFollowBall, rpc:beyonce.CmdWarpToStuff, rpc:beyonce.CmdWarpToStuffAutopilot
    let ctx = new_ctx();
    let mut sess = login_select(&ctx, C1, 8801).await;
    rpc(&ctx, &mut sess, "stationSvc", "Undock", &[]).await;
    let ship_id = sess.ship_id.unwrap();
    let bid = ctx.bind_object(&mut sess, "beyonce", i64::from(JITA)).unwrap();
    let h = ctx.systems.get_or_spawn(JITA);

    rpc_bound(&ctx, &mut sess, "beyonce", bid, "CmdSetSpeedFraction", &[PyRep::Real(0.75)]).await;
    assert_eq!(h.get_ball_snapshot(ship_id).await.unwrap().unwrap().1.speed_fraction, 0.75);

    rpc_bound(&ctx, &mut sess, "beyonce", bid, "CmdGotoDirection", &[PyRep::Real(1.0), PyRep::Real(0.0), PyRep::Real(0.0)]).await;
    assert!(matches!(h.get_ball_snapshot(ship_id).await.unwrap().unwrap().1.mode, MovementMode::GotoDirection { .. }));

    rpc_bound(&ctx, &mut sess, "beyonce", bid, "CmdSteerDirection", &[PyRep::Real(0.0), PyRep::Real(1.0), PyRep::Real(0.0)]).await;
    assert!(matches!(h.get_ball_snapshot(ship_id).await.unwrap().unwrap().1.mode, MovementMode::GotoDirection { direction } if direction.y > 0.9));

    rpc_bound(&ctx, &mut sess, "beyonce", bid, "CmdStop", &[]).await;
    assert_eq!(h.get_ball_snapshot(ship_id).await.unwrap().unwrap().1.mode, MovementMode::Stop);

    rpc_bound(&ctx, &mut sess, "beyonce", bid, "CmdAlignTo", &[PyRep::Integer(JITA_44)]).await;
    assert_eq!(h.get_ball_snapshot(ship_id).await.unwrap().unwrap().1.speed_fraction, 1.0);

    rpc_bound(&ctx, &mut sess, "beyonce", bid, "CmdOrbit", &[PyRep::Integer(JITA_44), PyRep::Real(5_000.0)]).await;
    assert!(matches!(h.get_ball_snapshot(ship_id).await.unwrap().unwrap().1.mode, MovementMode::Orbit { target_id: JITA_44, .. }));

    rpc_bound(&ctx, &mut sess, "beyonce", bid, "CmdFollowBall", &[PyRep::Integer(JITA_44), PyRep::Real(2_500.0)]).await;
    assert!(matches!(h.get_ball_snapshot(ship_id).await.unwrap().unwrap().1.mode, MovementMode::Follow { target_id: JITA_44, .. }));

    let gate_id = ctx.sde.jumps.values().find(|j| j.from_solar_system_id == JITA).expect("Jita stargate").stargate_id;
    rpc_bound(&ctx, &mut sess, "beyonce", bid, "CmdWarpToStuff", &[PyRep::String("item".into()), PyRep::Integer(gate_id), PyRep::Real(0.0)]).await;
    assert!(matches!(h.get_ball_snapshot(ship_id).await.unwrap().unwrap().1.mode, MovementMode::Warp { .. }));

    assert!(h.set_ball_position(ship_id, DVec3::ZERO).await);
    h.set_movement_mode(ship_id, MovementMode::Stop).await.unwrap();
    rpc_bound(&ctx, &mut sess, "beyonce", bid, "CmdWarpToStuffAutopilot", &[PyRep::String("item".into()), PyRep::Integer(gate_id)]).await;
    assert!(matches!(h.get_ball_snapshot(ship_id).await.unwrap().unwrap().1.mode, MovementMode::Warp { .. }));

    eve_testkit::attest(&[
        "rpc:beyonce.CmdSetSpeedFraction",
        "rpc:beyonce.CmdGotoDirection",
        "rpc:beyonce.CmdSteerDirection",
        "rpc:beyonce.CmdStop",
        "rpc:beyonce.CmdAlignTo",
        "rpc:beyonce.CmdOrbit",
        "rpc:beyonce.CmdFollowBall",
        "rpc:beyonce.CmdWarpToStuff",
        "rpc:beyonce.CmdWarpToStuffAutopilot",
    ]);
}

#[tokio::test]
async fn test_rpc_beyonce_stargate_dock_component_and_fleet_beacons() {
    // parity: rpc:beyonce.CmdDock, rpc:beyonce.CmdStargateJump, rpc:beyonce.CallComponentFromClient
    // parity: rpc:beyonce.CmdJumpToFleetModuleBeacon, rpc:beyonce.CmdBridgeToFleetModuleBeacon, rpc:beyonce.CmdJumpThroughFleet
    let ctx = new_ctx();
    let mut s1 = login_select(&ctx, C1, 8802).await;
    let mut s2 = login_select(&ctx, C2, 8803).await;
    let mut s3 = login_select(&ctx, C3, 8804).await;

    rpc(&ctx, &mut s1, "stationSvc", "Undock", &[]).await;
    let ship_id = s1.ship_id.unwrap();
    let bid_jita = ctx.bind_object(&mut s1, "beyonce", i64::from(JITA)).unwrap();
    let h_jita = ctx.systems.get_or_spawn(JITA);

    let trader_id = 995_001i64;
    seed_item(&ctx, trader_id, 86278, 1_000_125, i64::from(JITA), InventoryFlag::Hangar, 1);
    ctx.db.with_mut_snapshot(|s| { s.item_positions.insert(trader_id, [1_000.0, 0.0, 0.0]); });
    seed_item(&ctx, 995_002, 85316, C1, ship_id, InventoryFlag::Cargo, 30);
    assert!(h_jita.set_ball_position(ship_id, DVec3::new(1_000.0, 0.0, 0.0)).await);
    let trade = rpc_bound(&ctx, &mut s1, "beyonce", bid_jita, "CallComponentFromClient", &[
        PyRep::Integer(trader_id),
        PyRep::String("itemTrader".into()),
        PyRep::String("ProcessTrade".into()),
        PyRep::Integer(67),
        PyRep::Integer(1),
    ]).await;
    assert!(matches!(trade, PyRep::Tuple(ref v) if v.first() == Some(&PyRep::Bool(true))));

    assert!(h_jita.set_ball_position(ship_id, ctx.sde.stations[&JITA_44].position).await);
    rpc_bound(&ctx, &mut s1, "beyonce", bid_jita, "CmdDock", &[PyRep::Integer(JITA_44)]).await;
    assert_eq!(s1.station_id, Some(JITA_44));

    ctx.advance_virtual_clock(15);
    rpc(&ctx, &mut s1, "stationSvc", "Undock", &[]).await;
    let gate = ctx.sde.jumps.values().find(|j| j.from_solar_system_id == JITA).unwrap().clone();
    assert!(h_jita.set_ball_position(ship_id, gate.position).await);
    rpc_bound(&ctx, &mut s1, "beyonce", bid_jita, "CmdStargateJump", &[PyRep::Integer(gate.stargate_id)]).await;
    assert_eq!(s1.solar_system_id, Some(gate.to_solar_system_id));

    let mut low_sys: Vec<u32> = ctx.sde.solar_systems.values()
        .filter(|s| s.security_status < 0.4 && s.solar_system_id < 31_000_000 && s.galactic_pos_ly != [0.0; 3])
        .map(|s| s.solar_system_id)
        .collect();
    low_sys.sort_unstable();
    let (sys_a, sys_b) = low_sys.iter().copied().find_map(|a| {
        let pa = ctx.sde.solar_systems[&a].galactic_pos_ly;
        low_sys.iter().copied().find(|&b| {
            let pb = ctx.sde.solar_systems[&b].galactic_pos_ly;
            let d = ((pa[0] - pb[0]).powi(2) + (pa[1] - pb[1]).powi(2) + (pa[2] - pb[2]).powi(2)).sqrt();
            b != a && (0.3..1.8).contains(&d)
        }).map(|b| (a, b))
    }).unwrap();

    ctx.db.with_mut_snapshot(|s| {
        s.fleets.insert(8_800_001, eve_db::FleetRecord { fleet_id: 8_800_001, boss_id: C1, members: vec![C1, C2, C3], ..Default::default() });
    });
    board_in_space(&ctx, &mut s1, C1, 880_101, 11957, sys_b, DVec3::ZERO).await;
    grant_req_skills(&ctx, C1, 21096);
    seed_item(&ctx, 880_102, 21096, C1, 880_101, InventoryFlag::HiSlot0, 1);
    ctx.db.with_mut_snapshot(|s| { s.module_online_states.insert(880_102, true); });
    ctx.invalidate_ship_fitting(880_101);
    let ozone = ctx.sde.types[&21096].default_attributes[&713] as u32;
    seed_item(&ctx, 880_103, ozone, C1, 880_101, InventoryFlag::Cargo, 5_000);
    let cyno = rpc(&ctx, &mut s1, "dogmaIM", "Activate", &[PyRep::Integer(880_102), PyRep::String("cynosuralGeneration".into())]).await;
    let beacon_id = cyno.dict_get("beaconID").unwrap().as_i64().unwrap();
    let b_args = [PyRep::Integer(C1), PyRep::Integer(i64::from(sys_b)), PyRep::Integer(beacon_id)];

    board_in_space(&ctx, &mut s3, C3, 880_301, 19720, sys_a, DVec3::ZERO).await;
    let rev_fuel = ctx.sde.types[&19720].default_attributes[&866] as u32;
    seed_item(&ctx, 880_302, rev_fuel, C3, 880_301, InventoryFlag::Cargo, 100_000);
    let jmp = rpc(&ctx, &mut s3, "beyonce", "CmdJumpToFleetModuleBeacon", &b_args).await;
    assert_eq!(s3.solar_system_id, Some(sys_b));
    assert!(jmp.dict_get("fuelConsumed").unwrap().as_i64().unwrap() > 0);

    board_in_space(&ctx, &mut s2, C2, 880_201, 11567, sys_a, DVec3::ZERO).await;
    grant_req_skills(&ctx, C2, 23953);
    seed_item(&ctx, 880_202, 23953, C2, 880_201, InventoryFlag::HiSlot0, 1);
    ctx.db.with_mut_snapshot(|s| { s.module_online_states.insert(880_202, true); });
    ctx.invalidate_ship_fitting(880_201);
    let stront = ctx.sde.types[&23953].default_attributes[&713] as u32;
    let avatar_fuel = ctx.sde.types[&11567].default_attributes[&866] as u32;
    seed_item(&ctx, 880_203, stront, C2, 880_201, InventoryFlag::Cargo, 5_000);
    seed_item(&ctx, 880_204, avatar_fuel, C2, 880_201, InventoryFlag::Cargo, 500_000);
    let bridge = rpc(&ctx, &mut s2, "beyonce", "CmdBridgeToFleetModuleBeacon", &b_args).await;
    assert!(bridge.dict_get("activationConsumptionQuantity").unwrap().as_i64().unwrap() > 0);
    board_in_space(&ctx, &mut s3, C3, 880_305, 638, sys_a, DVec3::new(1_000.0, 0.0, 0.0)).await;
    eve_server::services::space_destiny::clear_session_change_timer(&ctx, C3);
    let jt = rpc(&ctx, &mut s3, "beyonce", "CmdJumpThroughFleet", &[PyRep::Integer(C2), PyRep::Integer(880_201)]).await;
    assert_eq!(s3.solar_system_id, Some(sys_b));
    assert!(jt.dict_get("fuelConsumed").unwrap().as_i64().unwrap() > 0);

    eve_testkit::attest(&[
        "rpc:beyonce.CmdDock",
        "rpc:beyonce.CmdStargateJump",
        "rpc:beyonce.CallComponentFromClient",
        "rpc:beyonce.CmdJumpToFleetModuleBeacon",
        "rpc:beyonce.CmdBridgeToFleetModuleBeacon",
        "rpc:beyonce.CmdJumpThroughFleet",
    ]);
}

#[tokio::test]
async fn test_rpc_dogma_im_info_targeting_and_probes() {
    // parity: rpc:dogmaIM.GetAllInfo, rpc:dogmaIM.ItemGetInfo, rpc:dogmaIM.AddTarget, rpc:dogmaIM.GetTargets
    // parity: rpc:dogmaIM.GetTargeters, rpc:dogmaIM.RemoveTarget, rpc:dogmaIM.ClearTargets, rpc:dogmaIM.LaunchProbes
    let ctx = new_ctx();
    let mut s1 = login_select(&ctx, C1, 8805).await;
    let mut s2 = login_select(&ctx, C2, 8806).await;

    let all_info = rpc(&ctx, &mut s1, "dogmaIM", "GetAllInfo", &[PyRep::Bool(true), PyRep::Bool(true)]).await;
    assert!(all_info.dict_get("shipInfo").is_some());
    let ship_id = s1.ship_id.unwrap();
    let item_info = rpc(&ctx, &mut s1, "dogmaIM", "ItemGetInfo", &[PyRep::Integer(ship_id)]).await;
    assert_eq!(item_info.dict_get("itemID"), Some(&PyRep::Integer(ship_id)));

    board_in_space(&ctx, &mut s1, C1, 881_001, 606, JITA, DVec3::ZERO).await;
    board_in_space(&ctx, &mut s2, C2, 881_002, 606, JITA, DVec3::new(2_000.0, 0.0, 0.0)).await;

    let add_res = rpc(&ctx, &mut s1, "dogmaIM", "AddTarget", &[PyRep::Integer(881_002)]).await;
    assert!(format!("{add_res:?}").contains("881002"));
    assert_eq!(rpc(&ctx, &mut s1, "dogmaIM", "GetTargets", &[]).await, PyRep::List(vec![PyRep::Integer(881_002)]));
    assert_eq!(rpc(&ctx, &mut s1, "dogmaIM", "GetTargeters", &[]).await, PyRep::List(vec![]));

    rpc(&ctx, &mut s1, "dogmaIM", "RemoveTarget", &[PyRep::Integer(881_002)]).await;
    assert_eq!(rpc(&ctx, &mut s1, "dogmaIM", "GetTargets", &[]).await, PyRep::List(vec![]));

    rpc(&ctx, &mut s1, "dogmaIM", "AddTarget", &[PyRep::Integer(881_002)]).await;
    rpc(&ctx, &mut s1, "dogmaIM", "ClearTargets", &[]).await;
    assert_eq!(rpc(&ctx, &mut s1, "dogmaIM", "GetTargets", &[]).await, PyRep::List(vec![]));

    grant_req_skills(&ctx, C1, 17938);
    seed_item(&ctx, 881_010, 17938, C1, 881_001, InventoryFlag::HiSlot0, 1);
    seed_item(&ctx, 881_011, 30013, C1, 881_001, InventoryFlag::HiSlot0, 8);
    ctx.db.with_mut_snapshot(|s| { s.module_online_states.insert(881_010, true); });
    ctx.invalidate_ship_fitting(881_001);
    let launched = rpc(&ctx, &mut s1, "dogmaIM", "LaunchProbes", &[]).await;
    assert_eq!(launched.dict_get("launchedCount"), Some(&PyRep::Integer(8)));

    eve_testkit::attest(&[
        "rpc:dogmaIM.GetAllInfo",
        "rpc:dogmaIM.ItemGetInfo",
        "rpc:dogmaIM.AddTarget",
        "rpc:dogmaIM.GetTargets",
        "rpc:dogmaIM.GetTargeters",
        "rpc:dogmaIM.RemoveTarget",
        "rpc:dogmaIM.ClearTargets",
        "rpc:dogmaIM.LaunchProbes",
    ]);
}

#[tokio::test]
async fn test_rpc_dogma_im_activation_overload_and_nanite_repair() {
    // parity: rpc:dogmaIM.Activate, rpc:dogmaIM.Overload, rpc:dogmaIM.StopOverload, rpc:dogmaIM.OverloadRack
    // parity: rpc:dogmaIM.StopOverloadRack, rpc:dogmaIM.Deactivate, rpc:dogmaIM.InitiateModuleRepair, rpc:dogmaIM.StopModuleRepair
    let ctx = new_ctx();
    let mut s1 = login_select(&ctx, C1, 8807).await;
    board_in_space(&ctx, &mut s1, C1, 882_001, 606, JITA, DVec3::ZERO).await;

    grant_req_skills(&ctx, C1, 439);
    seed_item(&ctx, 882_010, 439, C1, 882_001, InventoryFlag::MedSlot0, 1);
    ctx.db.with_mut_snapshot(|s| {
        s.module_online_states.insert(882_010, true);
        s.skills.entry(C1).or_default().insert(28609, TrainedSkillRecord { type_id: 28609, level: 4, skill_points: 100_000.0 });
    });
    ctx.invalidate_ship_fitting(882_001);

    let act = rpc(&ctx, &mut s1, "dogmaIM", "Activate", &[PyRep::Integer(882_010)]).await;
    assert_eq!(act.dict_get("moduleID"), Some(&PyRep::Integer(882_010)));

    let ov = rpc(&ctx, &mut s1, "dogmaIM", "Overload", &[PyRep::Integer(882_010)]).await;
    assert_eq!(ov.dict_get("moduleID"), Some(&PyRep::Integer(882_010)));

    let stop_ov = rpc(&ctx, &mut s1, "dogmaIM", "StopOverload", &[PyRep::Integer(882_010)]).await;
    assert_eq!(stop_ov.dict_get("overloaded"), Some(&PyRep::Bool(false)));

    let ov_rack = rpc(&ctx, &mut s1, "dogmaIM", "OverloadRack", &[PyRep::Integer(882_010)]).await;
    assert_eq!(ov_rack.dict_get("overloaded"), Some(&PyRep::Bool(true)));

    let stop_rack = rpc(&ctx, &mut s1, "dogmaIM", "StopOverloadRack", &[PyRep::Integer(882_010)]).await;
    assert_eq!(stop_rack.dict_get("overloaded"), Some(&PyRep::Bool(false)));

    let deact = rpc(&ctx, &mut s1, "dogmaIM", "Deactivate", &[PyRep::Integer(882_010)]).await;
    assert_eq!(deact.dict_get("moduleID"), Some(&PyRep::Integer(882_010)));

    ctx.db.with_mut_snapshot(|s| { s.items.get_mut(&882_010).unwrap().dynamic_attributes.insert(3, 12.0); });
    seed_item(&ctx, 882_011, 28668, C1, 882_001, InventoryFlag::Cargo, 50);
    ctx.invalidate_ship_fitting(882_001);

    let rep = rpc(&ctx, &mut s1, "dogmaIM", "InitiateModuleRepair", &[PyRep::Integer(882_010)]).await;
    assert!(rep.dict_get("pasteConsumed").unwrap().as_i64().unwrap() > 0);

    let stop_rep = rpc(&ctx, &mut s1, "dogmaIM", "StopModuleRepair", &[PyRep::Integer(882_010)]).await;
    assert_eq!(stop_rep.dict_get("moduleID"), Some(&PyRep::Integer(882_010)));

    eve_testkit::attest(&[
        "rpc:dogmaIM.Activate",
        "rpc:dogmaIM.Overload",
        "rpc:dogmaIM.StopOverload",
        "rpc:dogmaIM.OverloadRack",
        "rpc:dogmaIM.StopOverloadRack",
        "rpc:dogmaIM.Deactivate",
        "rpc:dogmaIM.InitiateModuleRepair",
        "rpc:dogmaIM.StopModuleRepair",
    ]);
}

#[tokio::test]
async fn test_rpc_dogma_im_implants_boosters_and_skill_injection() {
    // parity: rpc:dogmaIM.InjectImplant, rpc:dogmaIM.DestroyImplant, rpc:dogmaIM.UseBooster, rpc:dogmaIM.InjectSkillIntoBrain
    let ctx = new_ctx();
    let mut s1 = login_select(&ctx, C1, 8808).await;

    grant_req_skills(&ctx, C1, 9899);
    seed_item(&ctx, 883_001, 9899, C1, JITA_44, InventoryFlag::Hangar, 1);
    let inj = rpc(&ctx, &mut s1, "dogmaIM", "InjectImplant", &[PyRep::Integer(883_001)]).await;
    assert_eq!(inj, PyRep::None);
    assert_eq!(ctx.db.get_item(883_001).unwrap().flag, InventoryFlag::Implant);

    let dest = rpc(&ctx, &mut s1, "dogmaIM", "DestroyImplant", &[PyRep::Integer(883_001)]).await;
    assert_eq!(dest, PyRep::None);
    assert!(ctx.db.get_item(883_001).is_none());

    grant_req_skills(&ctx, C1, 28674);
    seed_item(&ctx, 883_002, 28674, C1, JITA_44, InventoryFlag::Hangar, 1);
    let boost = rpc(&ctx, &mut s1, "dogmaIM", "UseBooster", &[PyRep::Integer(883_002)]).await;
    assert_eq!(boost.dict_get("typeID"), Some(&PyRep::Integer(28674)));

    seed_item(&ctx, 883_003, 3412, C1, JITA_44, InventoryFlag::Hangar, 1);
    ctx.db.with_mut_snapshot(|s| { s.skills.entry(C1).or_default().remove(&3412); });
    let skill_res = rpc(&ctx, &mut s1, "dogmaIM", "InjectSkillIntoBrain", &[PyRep::List(vec![PyRep::Integer(883_003)])]).await;
    assert_eq!(skill_res.dict_get("typeID"), Some(&PyRep::Integer(3412)));
    assert!(ctx.db.with_snapshot(|s| s.skills[&C1].contains_key(&3412)));

    eve_testkit::attest(&[
        "rpc:dogmaIM.InjectImplant",
        "rpc:dogmaIM.DestroyImplant",
        "rpc:dogmaIM.UseBooster",
        "rpc:dogmaIM.InjectSkillIntoBrain",
    ]);
}

#[tokio::test]
async fn test_rpc_contract_proxy_lifecycle_and_auctions() {
    // parity: rpc:contractProxy.CreateContract, rpc:contractProxy.GetContract, rpc:contractProxy.GetLoginInfo
    // parity: rpc:contractProxy.AcceptContract, rpc:contractProxy.CompleteContract, rpc:contractProxy.PlaceBid, rpc:contractProxy.DeleteContract
    let ctx = new_ctx();
    let mut s1 = login_select(&ctx, C1, 8809).await;
    let mut s2 = login_select(&ctx, C2, 8810).await;

    seed_item(&ctx, 884_001, 34, C1, JITA_44, InventoryFlag::Hangar, 500);
    seed_item(&ctx, 884_002, 34, C1, JITA_44, InventoryFlag::Hangar, 500);
    seed_item(&ctx, 884_003, 34, C1, JITA_44, InventoryFlag::Hangar, 500);

    let courier_rep = rpc(&ctx, &mut s1, "contractProxy", "CreateContract", &[
        PyRep::Integer(3), PyRep::Integer(1), PyRep::Integer(C2), PyRep::Integer(1440), PyRep::Integer(1),
        PyRep::Integer(JITA_44), PyRep::Integer(JITA_44), PyRep::Real(0.0), PyRep::Real(100_000.0), PyRep::Real(50_000.0),
        PyRep::String("Courier Test".into()), PyRep::String("Desc".into()), PyRep::List(vec![PyRep::Integer(884_001)]),
    ]).await;
    let courier_id = courier_rep.as_i64().or_else(|| courier_rep.dict_get("contractID").and_then(|v| v.as_i64())).unwrap();
    assert!(courier_id > 0);

    let login_info = rpc(&ctx, &mut s2, "contractProxy", "GetLoginInfo", &[]).await;
    assert!(format!("{login_info:?}").contains(&courier_id.to_string()));

    let c_info = rpc(&ctx, &mut s1, "contractProxy", "GetContract", &[PyRep::Integer(courier_id)]).await;
    assert_eq!(c_info.dict_get("contract").and_then(|c| c.dict_get("title")), Some(&PyRep::String("Courier Test".into())));
    assert!(c_info.dict_get("items").is_some() && c_info.dict_get("bids").is_some());

    let acc = rpc(&ctx, &mut s2, "contractProxy", "AcceptContract", &[PyRep::Integer(courier_id)]).await;
    assert_eq!(acc.dict_get("status"), Some(&PyRep::String("InProgress".into())));

    let comp = rpc(&ctx, &mut s2, "contractProxy", "CompleteContract", &[PyRep::Integer(courier_id), PyRep::Integer(JITA_44), PyRep::Integer(1)]).await;
    assert_eq!(comp.dict_get("status"), Some(&PyRep::String("Completed".into())));

    let auction_rep = rpc(&ctx, &mut s1, "contractProxy", "CreateContract", &[
        PyRep::Integer(2), PyRep::Integer(0), PyRep::Integer(0), PyRep::Integer(1440), PyRep::Integer(1),
        PyRep::Integer(JITA_44), PyRep::Integer(JITA_44), PyRep::Real(100_000.0), PyRep::Real(0.0), PyRep::Real(500_000.0),
        PyRep::String("Auction Test".into()), PyRep::String("".into()), PyRep::List(vec![PyRep::Integer(884_002)]),
    ]).await;
    let auction_id = auction_rep.as_i64().or_else(|| auction_rep.dict_get("contractID").and_then(|v| v.as_i64())).unwrap();
    let bid_res = rpc(&ctx, &mut s2, "contractProxy", "PlaceBid", &[PyRep::Integer(auction_id), PyRep::Real(150_000.0)]).await;
    assert_eq!(bid_res.dict_get("currentBidISK"), Some(&PyRep::Real(150_000.0)));

    let del_rep = rpc(&ctx, &mut s1, "contractProxy", "CreateContract", &[
        PyRep::Integer(1), PyRep::Integer(0), PyRep::Integer(0), PyRep::Integer(1440), PyRep::Integer(1),
        PyRep::Integer(JITA_44), PyRep::Integer(JITA_44), PyRep::Real(50_000.0), PyRep::Real(0.0), PyRep::Real(0.0),
        PyRep::String("Delete Test".into()), PyRep::String("".into()), PyRep::List(vec![PyRep::Integer(884_003)]),
    ]).await;
    let del_id = del_rep.as_i64().or_else(|| del_rep.dict_get("contractID").and_then(|v| v.as_i64())).unwrap();
    let del_res = rpc(&ctx, &mut s1, "contractProxy", "DeleteContract", &[PyRep::Integer(del_id)]).await;
    assert_eq!(del_res.dict_get("status"), Some(&PyRep::String("Deleted".into())));

    eve_testkit::attest(&[
        "rpc:contractProxy.CreateContract",
        "rpc:contractProxy.GetContract",
        "rpc:contractProxy.GetLoginInfo",
        "rpc:contractProxy.AcceptContract",
        "rpc:contractProxy.CompleteContract",
        "rpc:contractProxy.PlaceBid",
        "rpc:contractProxy.DeleteContract",
    ]);
}

#[tokio::test]
async fn test_rpc_auth_cache_char_settings_and_login_services() {
    // parity: rpc:authentication.SetLanguageID, rpc:cache.GetServerVersionAndBuild, rpc:charMgr.GetCharacterSettings
    // parity: rpc:localizationServer.GetAllTextChanges, rpc:pingService.Ping, rpc:populationCap.GetCharacterLoadSlot
    // parity: rpc:skillMgr2.GetMySkillHandler, rpc:nes_intro.get_nes_intro_state, rpc:nes_intro.skip_nes_intro
    let ctx = new_ctx();
    let mut s1 = login_select(&ctx, C1, 8811).await;

    let lang = rpc(&ctx, &mut s1, "authentication", "SetLanguageID", &[PyRep::String("DE".into())]).await;
    assert_eq!(lang, PyRep::None);
    assert_eq!(s1.language_id, "DE");
    let ver = rpc(&ctx, &mut s1, "cache", "GetServerVersionAndBuild", &[]).await;
    assert_eq!(ver.dict_get("build"), Some(&PyRep::Integer(3_624_425)));
    assert_eq!(rpc(&ctx, &mut s1, "charMgr", "GetCharacterSettings", &[]).await, PyRep::Dict(vec![]));
    assert_eq!(rpc(&ctx, &mut s1, "localizationServer", "GetAllTextChanges", &[]).await, PyRep::None);
    let qa_err = ctx.dispatch_rpc(&mut s1, "localizationServer", None, "UpdateLocalizationQAWrap", &[PyRep::Dict(vec![(PyRep::Integer(665_412), PyRep::String("DE".into()))])], &HashMap::new(), None).await.unwrap_err();
    assert_eq!(qa_err.code, "RoleRequired");
    s1.role |= service_roles::ROLE_GMH;
    rpc(&ctx, &mut s1, "localizationServer", "UpdateLocalizationQAWrap", &[PyRep::Dict(vec![(PyRep::Integer(665_412), PyRep::String("DE".into()))])]).await;
    assert_eq!(rpc(&ctx, &mut s1, "localizationServer", "GetAllTextChanges", &[]).await, PyRep::Dict(vec![(PyRep::Integer(665_412), PyRep::String("DE".into()))]));
    assert!(rpc(&ctx, &mut s1, "pingService", "Ping", &[]).await.as_i64().unwrap() > 0);
    assert_eq!(rpc(&ctx, &mut s1, "populationCap", "GetCharacterLoadSlot", &[PyRep::Integer(C1)]).await, PyRep::Dict(vec![(PyRep::Integer(JITA as i64), PyRep::None)]));
    assert!(rpc(&ctx, &mut s1, "skillMgr2", "GetMySkillHandler", &[]).await.as_i64().unwrap() > 0);

    let nes0 = rpc(&ctx, &mut s1, "nes_intro", "get_nes_intro_state", &[]).await;
    assert_eq!(nes0, PyRep::Bool(false));
    let nes1 = rpc(&ctx, &mut s1, "nes_intro", "skip_nes_intro", &[]).await;
    assert_eq!(nes1, PyRep::None);
    assert_eq!(rpc(&ctx, &mut s1, "nes_intro", "get_nes_intro_state", &[]).await, PyRep::Bool(true));

    eve_testkit::attest(&[
        "rpc:authentication.SetLanguageID",
        "rpc:cache.GetServerVersionAndBuild",
        "rpc:charMgr.GetCharacterSettings",
        "rpc:localizationServer.GetAllTextChanges",
        "rpc:pingService.Ping",
        "rpc:populationCap.GetCharacterLoadSlot",
        "rpc:skillMgr2.GetMySkillHandler",
        "rpc:nes_intro.get_nes_intro_state",
        "rpc:nes_intro.skip_nes_intro",
    ]);
}

#[tokio::test]
async fn test_rpc_industry_insurance_market_reprocessing_and_orbitals() {
    // parity: rpc:blueprintManager.GetBlueprintData, rpc:industryMonitor.DisconnectJob, rpc:insuranceSvc.InsureShip
    // parity: rpc:insuranceSvc.UnInsureShip, rpc:marketProxy.CharGetTransactions, rpc:marketProxy.CorpGetTransactions
    // parity: rpc:reprocessingSvc.GetQuotes, rpc:reprocessingSvc.Reprocess, rpc:planetOrbitalRegistryBroker.RevertOrbitalsToInterBus
    let ctx = new_ctx();
    let mut s1 = login_select(&ctx, C1, 8812).await;
    let mut s2 = login_select(&ctx, C2, 8815).await;

    seed_item(&ctx, 885_001, 683, C1, JITA_44, InventoryFlag::Hangar, 1);
    let bp = rpc(&ctx, &mut s1, "blueprintManager", "GetBlueprintData", &[PyRep::Integer(885_001)]).await;
    assert_eq!(bp.dict_get("blueprintTypeID"), Some(&PyRep::Integer(683)));

    ctx.db.with_mut_snapshot(|s| {
        s.industry_jobs.insert(42, eve_db::IndustryJobRecord {
            job_id: 42, installer_id: C1, facility_id: JITA_44, blueprint_item_id: 885_001, blueprint_type_id: 683,
            activity_id: 1, runs: 1, product_type_id: 587, status: "Active".into(), duration_seconds: 600, start_unix: 0,
        });
    });
    assert_eq!(ctx.dispatch_rpc(&mut s2, "industryMonitor", None, "ConnectJob", &[PyRep::Integer(42)], &HashMap::new(), None).await.unwrap_err().code, "CrpAccessDenied");
    rpc(&ctx, &mut s1, "industryMonitor", "ConnectJob", &[PyRep::Integer(42)]).await;
    let disc = rpc(&ctx, &mut s1, "industryMonitor", "DisconnectJob", &[PyRep::Integer(42)]).await;
    assert_eq!(disc.dict_get("unsubscribed"), Some(&PyRep::Bool(true)));
    assert_eq!(disc.dict_get("connected"), Some(&PyRep::Bool(false)));

    let ship_id = s1.ship_id.unwrap();
    let ins = rpc(&ctx, &mut s1, "insuranceSvc", "InsureShip", &[PyRep::Integer(ship_id), PyRep::Real(1.0)]).await;
    assert!(ins.dict_get("payoutISK").unwrap().as_f64().unwrap() > 0.0);
    let unins = rpc(&ctx, &mut s1, "insuranceSvc", "UnInsureShip", &[PyRep::Integer(ship_id)]).await;
    assert_eq!(unins.dict_get("shipID"), Some(&PyRep::Integer(ship_id)));

    let char_tx = rpc(&ctx, &mut s1, "marketProxy", "CharGetTransactions", &[]).await;
    assert!(format!("{char_tx:?}").contains("transactionID"));
    ctx.db.with_mut_snapshot(|s| {
        let cid = 98_000_077;
        s.characters.get_mut(&C1).unwrap().corporation_id = cid;
        s.characters.get_mut(&C2).unwrap().corporation_id = cid;
        s.corporations.insert(cid, eve_db::CorporationRecord {
            corporation_id: cid, name: "TC".into(), ticker: "TC".into(), ceo_id: C1, alliance_id: None, tax_rate: 0.05, master_wallet: eve_db::IskCents(0),
            members: Default::default(), member_roles: Default::default(), applications: Default::default(),
            wallet_divisions: eve_db::CorporationRecord::default_wallet_divisions(eve_db::IskCents(0)), description: String::new(), url: String::new(), hq_station_id: Some(JITA_44),
        });
        eve_server::services::industry_pi::save_orbital_settings(s, 40_009_082, &eve_server::services::industry_pi::OrbitalCustomsSettings { planet_id: 40_009_082, owner_corp_id: cid, ..Default::default() });
        s.industry_jobs.insert(43, eve_db::IndustryJobRecord {
            job_id: 43, installer_id: C1, facility_id: JITA_44, blueprint_item_id: 885_001, blueprint_type_id: 683,
            activity_id: 1, runs: 1, product_type_id: 587, status: "Active".into(), duration_seconds: 600, start_unix: 0,
        });
        eve_server::services::industry_pi::save_job_cost_meta(s, 43, &eve_server::services::industry_pi::IndustryJobCostMeta {
            use_corp: true, owner_id: cid, ..Default::default()
        });
    });
    s1.corporation_id = Some(98_000_077);
    s2.corporation_id = Some(98_000_077);
    // Unprivileged corp member (C2 has no roles) is denied access to corp job 43
    assert_eq!(ctx.dispatch_rpc(&mut s2, "industryMonitor", None, "ConnectJob", &[PyRep::Integer(43)], &HashMap::new(), None).await.unwrap_err().code, "CrpAccessDenied");
    // Granting ROLE_FACTORY_MANAGER allows C2 to connect and disconnect
    ctx.db.with_mut_snapshot(|s| {
        s.corporations.get_mut(&98_000_077).unwrap().member_roles.insert(C2, eve_db::corp_roles::ROLE_FACTORY_MANAGER);
    });
    rpc(&ctx, &mut s2, "industryMonitor", "ConnectJob", &[PyRep::Integer(43)]).await;
    rpc(&ctx, &mut s2, "industryMonitor", "DisconnectJob", &[PyRep::Integer(43)]).await;
    let corp_tx = rpc(&ctx, &mut s1, "marketProxy", "CorpGetTransactions", &[]).await;
    assert!(format!("{corp_tx:?}").contains("transactionID"));

    seed_item(&ctx, 885_002, 1230, C1, JITA_44, InventoryFlag::Hangar, 1_000);
    let quotes = rpc(&ctx, &mut s1, "reprocessingSvc", "GetQuotes", &[PyRep::List(vec![PyRep::Integer(885_002)])]).await;
    assert!(format!("{quotes:?}").contains("34"));
    let reproc = rpc(&ctx, &mut s1, "reprocessingSvc", "Reprocess", &[PyRep::Integer(885_002)]).await;
    assert!(format!("{reproc:?}").contains("34"));

    let upd = rpc(&ctx, &mut s1, "planetOrbitalRegistryBroker", "UpdateSettings", &[PyRep::Integer(40_009_082), PyRep::Integer(12), PyRep::keyval([("corporation", PyRep::Real(0.03))])]).await;
    assert_eq!(upd.dict_get("corporationTaxRate"), Some(&PyRep::Real(0.03)));
    let unauth_upd = ctx.dispatch_rpc(&mut s2, "planetOrbitalRegistryBroker", None, "UpdateSettings", &[PyRep::Integer(40_009_082)], &HashMap::new(), None).await.unwrap_err();
    assert_eq!(unauth_upd.code, "CrpAccessDenied");
    let unauth_rev = ctx.dispatch_rpc(&mut s2, "planetOrbitalRegistryBroker", None, "RevertOrbitalsToInterBus", &[PyRep::Integer(40_009_082)], &HashMap::new(), None).await.unwrap_err();
    assert_eq!(unauth_rev.code, "CrpAccessDenied");
    let rev = rpc(&ctx, &mut s1, "planetOrbitalRegistryBroker", "RevertOrbitalsToInterBus", &[PyRep::Integer(40_009_082)]).await;
    assert_eq!(rev.dict_get("ownerCorpID"), Some(&PyRep::Integer(1_000_148)));

    eve_testkit::attest(&[
        "rpc:blueprintManager.GetBlueprintData",
        "rpc:industryMonitor.DisconnectJob",
        "rpc:insuranceSvc.InsureShip",
        "rpc:insuranceSvc.UnInsureShip",
        "rpc:marketProxy.CharGetTransactions",
        "rpc:marketProxy.CorpGetTransactions",
        "rpc:reprocessingSvc.GetQuotes",
        "rpc:reprocessingSvc.Reprocess",
        "rpc:planetOrbitalRegistryBroker.RevertOrbitalsToInterBus",
    ]);
}

#[tokio::test]
async fn test_rpc_map_agency_presets_and_shared_settings() {
    // parity: rpc:map.GetConstellationLPData, rpc:map.GetDeadspaceAgentsMap, rpc:map.GetHistory
    // parity: rpc:custom_agency_provider.get_content_data, rpc:overviewPresetMgr.StoreLinkAndGetID
    // parity: rpc:overviewPresetMgr.GetStoredPreset, rpc:sharedSettingsMgr.StoreSettingLinkAndGetID, rpc:sharedSettingsMgr.GetStoredSharedSetting
    let ctx = new_ctx();
    let mut s1 = login_select(&ctx, C1, 8813).await;

    let lp = rpc(&ctx, &mut s1, "map", "GetConstellationLPData", &[PyRep::Integer(20_000_020)]).await;
    assert!(format!("{lp:?}").contains("500001"));
    ctx.db.with_mut_snapshot(|s| { s.fw_system_occupations.insert(30_000_142, eve_db::FwSystemOccupationRecord { solar_system_id: 30_000_142, occupier_faction_id: 500_004, owner_faction_id: 500_001, victory_points: 1200, victory_points_threshold: 3000 }); });
    let lp_fw = rpc(&ctx, &mut s1, "map", "GetConstellationLPData", &[PyRep::Integer(20_000_020)]).await;
    assert!(format!("{lp_fw:?}").contains("500004"));
    let agents = rpc(&ctx, &mut s1, "map", "GetDeadspaceAgentsMap", &[]).await;
    assert!(agents.crowset_rows().unwrap().len() > 32);
    let hist = rpc(&ctx, &mut s1, "map", "GetHistory", &[PyRep::Integer(3), PyRep::Integer(1)]).await;
    assert!(!hist.crowset_rows().unwrap().is_empty());

    assert!(!format!("{:?}", rpc(&ctx, &mut s1, "custom_agency_provider", "get_content_data", &[]).await).is_empty());

    let PyRep::KeyVal(p_kv) = rpc(&ctx, &mut s1, "overviewPresetMgr", "StoreLinkAndGetID", &[PyRep::String("{\"name\":\"PvP\"}".into())]).await else { panic!() };
    assert_eq!(rpc(&ctx, &mut s1, "overviewPresetMgr", "GetStoredPreset", &[p_kv["sqID"].clone()]).await, PyRep::String("{\"name\":\"PvP\"}".into()));
    let PyRep::KeyVal(s_kv) = rpc(&ctx, &mut s1, "sharedSettingsMgr", "StoreSettingLinkAndGetID", &[PyRep::String("Brackets".into()), PyRep::String("{\"brackets\":true}".into())]).await else { panic!() };
    assert_eq!(rpc(&ctx, &mut s1, "sharedSettingsMgr", "GetStoredSharedSetting", &[s_kv["linkID"].clone()]).await, PyRep::String("{\"brackets\":true}".into()));
    eve_testkit::attest(&[
        "rpc:map.GetConstellationLPData",
        "rpc:map.GetDeadspaceAgentsMap",
        "rpc:map.GetHistory",
        "rpc:custom_agency_provider.get_content_data",
        "rpc:overviewPresetMgr.StoreLinkAndGetID",
        "rpc:overviewPresetMgr.GetStoredPreset",
        "rpc:sharedSettingsMgr.StoreSettingLinkAndGetID",
        "rpc:sharedSettingsMgr.GetStoredSharedSetting",
    ]);
}

#[tokio::test]
async fn test_rpc_survey_interference_drones_qa_and_filaments() {
    // DATA GAP: requires live survey catalog import
    // parity: rpc:solarsystemInterferenceMgr.GetAllInterferenceBands, rpc:ship.ScoopDrone
    // parity: rpc:publicQaToolsServer.SlashCmd, rpc:randomJumpMgr.ActivateRandomJumpFilament
    let ctx = new_ctx();
    let mut s1 = login_select(&ctx, C1, 8814).await;

    assert_eq!(rpc(&ctx, &mut s1, "survey", "GetActiveSurveyID", &[]).await, PyRep::None);
    let now = ctx.now_unix();
    eve_server::services::operations_manager::register_survey(&ctx, eve_server::services::operations_manager::SurveyDefinition { survey_id: 2801, name: "Ops Survey".into(), description: "Active".into(), start_date: None, end_date: None, start_unix: Some(now - 3600), expires_at_unix: Some(now + 86_400), grace_period_days: 7, min_account_age_hours: 24, item_rewards: vec![(34, 100)], vgs_rewards: vec![5003] });
    assert_eq!(rpc(&ctx, &mut s1, "survey", "PerformSurveyChecks", &[]).await, PyRep::List(vec![PyRep::Integer(2801)]));
    assert_eq!(rpc(&ctx, &mut s1, "survey", "GetActiveSurveyID", &[]).await, PyRep::Integer(2801));
    let claim = rpc(&ctx, &mut s1, "survey", "ClaimSurveyRewards", &[PyRep::Integer(2801)]).await;
    assert_eq!(claim, PyRep::Bool(true));

    let bands = rpc(&ctx, &mut s1, "solarsystemInterferenceMgr", "GetAllInterferenceBands", &[]).await;
    assert!(format!("{bands:?}").contains("30000142"));

    s1.role |= service_roles::ROLE_GMH | service_roles::ROLE_QA;
    let qa = rpc(&ctx, &mut s1, "publicQaToolsServer", "SlashCmd", &[PyRep::String("/location".into())]).await;
    assert_eq!(qa.dict_get("characterID"), Some(&PyRep::Integer(C1)));

    board_in_space(&ctx, &mut s1, C1, 886_001, 606, JITA, DVec3::ZERO).await;
    seed_item(&ctx, 886_002, 2454, C1, 886_001, InventoryFlag::DroneBay, 1);
    let launched = rpc(&ctx, &mut s1, "ship", "LaunchDrones", &[PyRep::List(vec![PyRep::Integer(886_002)])]).await;
    let PyRep::List(d_list) = launched else { panic!("expected drone list") };
    let drone_ball_id = d_list[0].as_i64().unwrap();
    let scooped = rpc(&ctx, &mut s1, "ship", "ScoopDrone", &[PyRep::Integer(drone_ball_id)]).await;
    assert_eq!(scooped.dict_get("scoopedDroneItemID"), Some(&PyRep::Integer(886_002)));

    ctx.db.with_mut_snapshot(|s| { s.fleets.insert(886099, eve_db::FleetRecord { fleet_id: 886099, boss_id: C1, members: vec![C1], ..Default::default() }); });
    let fil_tid = *ctx.sde.types.values().find(|t| t.group_id == 4041 && t.default_attributes.contains_key(&3025)).map(|t| &t.type_id).unwrap();
    seed_item(&ctx, 886_003, fil_tid, C1, 886_001, InventoryFlag::Cargo, 1);
    let jump = rpc(&ctx, &mut s1, "randomJumpMgr", "ActivateRandomJumpFilament", &[PyRep::Integer(886_003)]).await;
    assert_ne!(s1.solar_system_id, Some(JITA));
    assert!(jump.dict_get("solarSystemID").is_some());

    eve_testkit::attest(&[
        "rpc:solarsystemInterferenceMgr.GetAllInterferenceBands", "rpc:ship.ScoopDrone",
        "rpc:publicQaToolsServer.SlashCmd", "rpc:randomJumpMgr.ActivateRandomJumpFilament",
    ]);
}
