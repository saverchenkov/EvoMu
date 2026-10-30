//! End-to-end integration tests for `feat:lifecycle.*` parity rows (Batch 30, Part 1).
//!
//! Covers:
//! - `feat:lifecycle.fleet-kick-session`, `feat:lifecycle.fleet-login-consistency`,
//!   `feat:lifecycle.fleet-logout`, `feat:lifecycle.fleet-server-restart`
//! - `feat:lifecycle.industry-pause-and-many`, `feat:lifecycle.owner_notes-persistence`
//! - `feat:lifecycle.industry-pi-durable`, `feat:lifecycle.market-wallet-history-durable`

#![forbid(unsafe_code)]

use crate::common;
use eve_db::{
    corp_roles, CorporationRecord, InventoryFlag, InventoryItemRecord, IskCents,
    TrainedSkillRecord, UpwellStructureRecord, VirtualServerClock, WorldRepository,
};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use eve_server::services::industry_pi::{
    load_job_cost_meta, save_orbital_settings, OrbitalCustomsSettings,
};
use eve_server::services::station_structure::structure_lifecycle::sync_all_structures;
use eve_server::services::wallet_serverops::create_bill;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

const P1: i64 = 90_000_001;
const P2: i64 = 90_000_002;
const CORP_ID: i64 = 2001;
const JITA_SYSTEM_ID: u32 = 30_000_142;
const JITA_STATION_ID: i64 = 60_003_760;
const JITA_PLANET_ID: i64 = 40_009_082;
const FH: &str = "fleetObjectHandler";

fn temp_db_path(tag: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!("evomu_b30_{tag}_{}.sqlite", std::process::id()));
    let _ = std::fs::remove_file(&p);
    p
}

fn open_fresh_ctx_at(path: &Path, base_unix: u64) -> ServerContext {
    let demo = WorldRepository::with_demo_seed().with_snapshot(|s| s.clone());
    let repo = WorldRepository::open_sqlite(path).expect("open_sqlite");
    repo.with_mut_snapshot(|s| *s = demo);
    let ctx = ServerContext::with_repository(Arc::new(repo));
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(base_unix)));
    ctx
}

fn open_ctx_at(path: &Path, base_unix: u64) -> ServerContext {
    let repo = WorldRepository::open_sqlite(path).expect("open_sqlite");
    let ctx = ServerContext::with_repository(Arc::new(repo));
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(base_unix)));
    ctx
}

async fn rpc(ctx: &ServerContext, sess: &mut SessionState, svc: &str, method: &str, args: &[PyRep]) -> Result<PyRep, BridgeRpcError> {
    ctx.dispatch_rpc(sess, svc, None, method, args, &HashMap::new(), None).await
}

async fn rpc_kw(
    ctx: &ServerContext,
    sess: &mut SessionState,
    svc: &str,
    method: &str,
    args: &[PyRep],
    kw: &[(&str, PyRep)],
) -> Result<PyRep, BridgeRpcError> {
    let kwargs: HashMap<String, PyRep> = kw.iter().map(|(k, v)| ((*k).to_string(), v.clone())).collect();
    ctx.dispatch_rpc(sess, svc, None, method, args, &kwargs, None).await
}

async fn login_select(ctx: &ServerContext, sid: u64, user: &str, pass: &str, char_id: i64) -> SessionState {
    let mut sess = SessionState::new(sid);
    common::login_pilot(ctx, &mut sess, user, pass).await;
    rpc(ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(char_id)]).await.expect("SelectCharacterID");
    ctx.advance_secs(12);
    sess
}

fn rowset_maps(rep: &PyRep) -> Vec<HashMap<String, PyRep>> {
    let PyRep::Rowset { columns, rows } = rep else { panic!("expected Rowset, got {rep:?}") };
    rows.iter().map(|r| columns.iter().cloned().zip(r.iter().cloned()).collect()).collect()
}

fn make_item(item_id: i64, type_id: u32, owner_id: i64, location_id: i64, flag: InventoryFlag, quantity: i32, singleton: bool) -> InventoryItemRecord {
    InventoryItemRecord { item_id, type_id, owner_id, location_id, flag, quantity, singleton, version: 1, ..Default::default() }
}

fn seed_corp_full(snap: &mut eve_db::WorldStateSnapshot, char_id: i64, corp_id: i64) {
    let roles = corp_roles::ROLE_DIRECTOR | corp_roles::ROLE_ACCOUNTANT | corp_roles::ROLE_STATION_MANAGER | corp_roles::ROLE_STARBASE_CONFIG;
    let mut corp = CorporationRecord {
        corporation_id: corp_id, name: format!("B30 Corp {corp_id}"), ticker: "B30".into(), ceo_id: char_id,
        hq_station_id: Some(JITA_STATION_ID), members: [char_id].into_iter().collect(),
        member_roles: HashMap::from([(char_id, roles)]), ..Default::default()
    };
    corp.ensure_wallet_divisions();
    for d in &mut corp.wallet_divisions {
        d.balance = IskCents::from_isk(50_000_000);
    }
    corp.master_wallet = IskCents::from_isk(50_000_000);
    snap.corporations.insert(corp_id, corp);
    if let Some(ch) = snap.characters.get_mut(&char_id) {
        ch.corporation_id = corp_id;
        ch.balance = IskCents::from_isk(100_000_000);
    }
}

fn pi_cmd(op: i64, a: i64, b: i64, c: i64) -> PyRep {
    PyRep::Tuple(vec![PyRep::Integer(op), PyRep::Integer(a), PyRep::Integer(b), PyRep::Integer(c)])
}

#[tokio::test]
async fn test_b30_fleet_lifecycle_kick_login_logout_and_restart() {
    // parity: feat:lifecycle.fleet-kick-session, feat:lifecycle.fleet-login-consistency, feat:lifecycle.fleet-logout, feat:lifecycle.fleet-server-restart
    eve_testkit::attest(&[
        "feat:lifecycle.fleet-kick-session",
        "feat:lifecycle.fleet-login-consistency",
        "feat:lifecycle.fleet-logout",
        "feat:lifecycle.fleet-server-restart",
    ]);

    let db_path = temp_db_path("fleet");
    let ctx = open_fresh_ctx_at(&db_path, 1_760_000_000);
    let uid3 = ctx.db.provision_player_account("b30_fl3", "b30_fl3_pass");
    let p3 = ctx.db.create_character(uid3, "B30 Fleet 3", 1, 1, 11, 17, &ctx.sde).unwrap().character_id;

    let mut s1 = login_select(&ctx, 9101, "demo", "demo-secret", P1).await;
    let mut s2 = login_select(&ctx, 9102, "other", "other-secret", P2).await;
    let mut s3 = login_select(&ctx, 9103, "b30_fl3", "b30_fl3_pass", p3).await;

    let fid = rpc(&ctx, &mut s1, "fleetMgr", "CreateFleet", &[]).await.unwrap().dict_get("fleetID").and_then(|v| v.as_i64()).unwrap();
    let wid = rpc(&ctx, &mut s1, FH, "CreateWing", &[PyRep::String("Wing 1".into())]).await.unwrap().dict_get("wingID").and_then(|v| v.as_i64()).unwrap();
    let sid = rpc(&ctx, &mut s1, FH, "CreateSquad", &[PyRep::Integer(wid), PyRep::String("Squad 1".into())]).await.unwrap().dict_get("squadID").and_then(|v| v.as_i64()).unwrap();

    // `fleet-login-consistency`: CreateFleet, AcceptInvite, ApplyToJoinFleet refuse character already in a fleet; GetFleetID agrees
    let fid_b = rpc(&ctx, &mut s3, "fleetMgr", "CreateFleet", &[]).await.unwrap().dict_get("fleetID").and_then(|v| v.as_i64()).unwrap();
    rpc(&ctx, &mut s3, FH, "Invite", &[PyRep::Integer(P2)]).await.unwrap();
    assert_eq!(rpc(&ctx, &mut s1, "fleetMgr", "CreateFleet", &[]).await.unwrap_err().code, "AlreadyInFleet");
    assert_eq!(rpc(&ctx, &mut s1, FH, "AcceptInvite", &[PyRep::Integer(fid_b)]).await.unwrap_err().code, "AlreadyInFleet");
    assert_eq!(rpc(&ctx, &mut s1, "fleetProxy", "ApplyToJoinFleet", &[PyRep::Integer(fid_b)]).await.unwrap_err().code, "AlreadyInFleet");
    assert_eq!(rpc(&ctx, &mut s1, FH, "GetFleetID", &[]).await, Ok(PyRep::Integer(fid)));
    assert_eq!(s1.fleet_id, Some(fid));
    ctx.db.with_snapshot(|snap| assert!(snap.fleets[&fid].members.contains(&P1)));
    rpc(&ctx, &mut s3, "authentication", "Logout", &[]).await.unwrap();

    // `fleet-kick-session`: KickMember pushes OnSessionChanged clearing fleetid and blocks kicked member's RPCs
    rpc(&ctx, &mut s1, FH, "Invite", &[PyRep::Integer(P2), PyRep::Integer(wid), PyRep::None, PyRep::Integer(2)]).await.unwrap();
    eve_server::services::space_destiny::clear_session_change_timer(&ctx, P2);
    rpc(&ctx, &mut s2, FH, "AcceptInvite", &[PyRep::Integer(fid)]).await.unwrap();
    assert_eq!(s2.fleet_id, Some(fid));
    ctx.drain_notifications_for_char(P2);
    rpc(&ctx, &mut s1, FH, "KickMember", &[PyRep::Integer(P2)]).await.unwrap();
    ctx.db.with_snapshot(|snap| assert_eq!(snap.characters[&P2].fleet_id, None));
    assert!(ctx.drain_notifications_for_char(P2).iter().any(|n| {
        n.event == "OnSessionChanged"
            && n.args.iter().any(|a| a.dict_get("fleetid") == Some(&PyRep::Tuple(vec![PyRep::Integer(fid), PyRep::None])))
    }));
    assert_eq!(rpc(&ctx, &mut s2, FH, "Invite", &[PyRep::Integer(p3)]).await.unwrap_err().code, "NotInFleet");
    assert_eq!(rpc(&ctx, &mut s2, FH, "SendBroadcast", &[PyRep::String("Target".into()), PyRep::Integer(0)]).await.unwrap_err().code, "NotInFleet");

    // `fleet-logout`: Logout removes from slots/channel, emits OnFleetLeave, passes boss, and disbands empty fleet
    rpc(&ctx, &mut s3, "authentication", "Logout", &[]).await.unwrap();
    let mut s3 = login_select(&ctx, 9105, "b30_fl3", "b30_fl3_pass", p3).await;
    rpc(&ctx, &mut s1, FH, "Invite", &[PyRep::Integer(P2), PyRep::Integer(wid), PyRep::None, PyRep::Integer(2)]).await.unwrap();
    eve_server::services::space_destiny::clear_session_change_timer(&ctx, P2);
    rpc(&ctx, &mut s2, FH, "AcceptInvite", &[PyRep::Integer(fid)]).await.unwrap();
    rpc(&ctx, &mut s1, FH, "Invite", &[PyRep::Integer(p3), PyRep::Integer(wid), PyRep::Integer(sid), PyRep::Integer(3)]).await.unwrap();
    eve_server::services::space_destiny::clear_session_change_timer(&ctx, p3);
    rpc(&ctx, &mut s3, FH, "AcceptInvite", &[PyRep::Integer(fid)]).await.unwrap();
    for c in [P1, P2, p3] {
        ctx.drain_notifications_for_char(c);
    }

    rpc(&ctx, &mut s3, "authentication", "Logout", &[]).await.unwrap();
    ctx.db.with_snapshot(|snap| {
        let f = &snap.fleets[&fid];
        assert!(!f.members.contains(&p3) && !f.member_details.contains_key(&p3));
        assert_eq!(f.squad_details[&sid].commander_id, None);
        assert!(!snap.chat_channels[&format!("fleetid:{fid}")].members.contains(&p3));
    });
    assert!(ctx.drain_notifications_for_char(P1).iter().any(|n| n.event == "OnFleetLeave" && n.args == vec![PyRep::Integer(p3)]));

    rpc(&ctx, &mut s1, "authentication", "Logout", &[]).await.unwrap();
    ctx.db.with_snapshot(|snap| {
        let f = &snap.fleets[&fid];
        assert_eq!(f.boss_id, P2);
        assert_ne!(f.member_details[&P2].job & 2, 0);
    });
    let p2_notifs = ctx.drain_notifications_for_char(P2);
    assert!(p2_notifs.iter().any(|n| n.event == "OnFleetLeave" && n.args == vec![PyRep::Integer(P1)]));
    assert!(p2_notifs.iter().any(|n| n.event == "OnFleetMemberChanged" && n.args.get(7).and_then(|v| v.as_i64()).is_some_and(|j| (j & 2) != 0)));

    rpc(&ctx, &mut s2, "authentication", "Logout", &[]).await.unwrap();
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.fleets.contains_key(&fid) && !snap.chat_channels.contains_key(&format!("fleetid:{fid}")));
    });

    // `fleet-server-restart`: Server restart drops offline fleets so no ghost members survive
    let mut s2_new = login_select(&ctx, 9106, "other", "other-secret", P2).await;
    let fid2 = rpc(&ctx, &mut s2_new, "fleetMgr", "CreateFleet", &[]).await.unwrap().dict_get("fleetID").and_then(|v| v.as_i64()).unwrap();
    drop(ctx);
    let ctx2 = open_ctx_at(&db_path, 1_760_001_000);
    let mut s2_relog = login_select(&ctx2, 9107, "other", "other-secret", P2).await;
    assert_eq!(s2_relog.fleet_id, None);
    assert_eq!(rpc(&ctx2, &mut s2_relog, FH, "GetFleetID", &[]).await.unwrap(), PyRep::None);
    ctx2.db.with_snapshot(|snap| assert!(!snap.fleets.contains_key(&fid2)));
    let _ = std::fs::remove_file(&db_path);
}

#[tokio::test]
async fn test_b30_industry_pause_many_and_owner_notes_persistence() {
    // parity: feat:lifecycle.industry-pause-and-many, feat:lifecycle.owner_notes-persistence
    eve_testkit::attest(&[
        "feat:lifecycle.industry-pause-and-many",
        "feat:lifecycle.owner_notes-persistence",
    ]);

    let db_path = temp_db_path("industry_owner");
    let base_u = 1_760_000_000u64;
    let ctx = open_fresh_ctx_at(&db_path, base_u);
    let struct_id = 1_000_000_251i64;

    ctx.db.with_mut_snapshot(|snap| {
        seed_corp_full(snap, P1, CORP_ID);
        if let Some(ch) = snap.characters.get_mut(&P1) {
            ch.station_id = None;
            ch.structure_id = Some(struct_id);
            ch.solar_system_id = JITA_SYSTEM_ID;
        }
        for &(tid, lvl) in &[(3380, 5), (3387, 5), (2495, 5), (2505, 5), (3443, 5), (3447, 5), (16596, 5), (13279, 4), (2406, 3), (2403, 2)] {
            snap.skills.entry(P1).or_default().insert(tid, TrainedSkillRecord { type_id: tid, level: lvl, skill_points: 256_000.0 });
        }
        snap.structures.insert(struct_id, UpwellStructureRecord {
            structure_id: struct_id, type_id: 35826, owner_corp_id: CORP_ID, solar_system_id: JITA_SYSTEM_ID,
            name: "B30 Azbel".into(), position: [0.0, 0.0, 0.0], state: "ShieldVulnerable".into(),
            quantum_core_installed: true, fuel_blocks: 5_000, shield_hp: 10000.0, armor_hp: 10000.0,
            hull_hp: 10000.0, reinforced_until_unix: 0, last_damage_tick: 0, last_damage_unix: 0, self_destruct_at_unix: None,
        });
        let mut svc = make_item(struct_id + 1, 35878, CORP_ID, struct_id, InventoryFlag::ServiceSlot0, 1, true);
        svc.dynamic_attributes.insert(2, 1.0);
        snap.items.insert(struct_id + 1, svc);
        snap.module_online_states.insert(struct_id + 1, true);
        for iid in [710_001, 710_002, 710_003, 710_004, 710_005] {
            snap.items.insert(iid, make_item(iid, 785, P1, struct_id, InventoryFlag::Hangar, 1, true));
        }
        for (iid, tid, qty) in [(710_010, 34u32, 5_000_000), (710_011, 35u32, 1_000_000), (710_012, 36u32, 500_000)] {
            snap.items.insert(iid, make_item(iid, tid, P1, struct_id, InventoryFlag::Hangar, qty, false));
        }
        snap.items.insert(710_020, make_item(710_020, 691, CORP_ID, JITA_STATION_ID, InventoryFlag::Hangar, 1, true));
        snap.items.insert(710_021, make_item(710_021, 34, CORP_ID, JITA_STATION_ID, InventoryFlag::Hangar, 100_000, false));
    });

    let mut sess = login_select(&ctx, 9201, "demo", "demo-secret", P1).await;
    sync_all_structures(&ctx);
    ctx.db.with_mut_snapshot(|snap| snap.structures.get_mut(&struct_id).unwrap().fuel_blocks = 1);

    let install_mfg = |bp_id: i64, runs: i64| PyRep::Dict(vec![
        (PyRep::String("blueprintID".into()), PyRep::Integer(bp_id)),
        (PyRep::String("activityID".into()), PyRep::Integer(1)),
        (PyRep::String("runs".into()), PyRep::Integer(runs)),
    ]);
    let j1 = rpc(&ctx, &mut sess, "industryManager", "InstallJob", &[install_mfg(710_001, 1)]).await.unwrap().dict_get("jobID").and_then(|v| v.as_i64()).unwrap();
    let j2 = rpc(&ctx, &mut sess, "industryManager", "InstallJob", &[install_mfg(710_002, 1)]).await.unwrap().dict_get("jobID").and_then(|v| v.as_i64()).unwrap();
    let j_long = rpc(&ctx, &mut sess, "industryManager", "InstallJob", &[install_mfg(710_003, 10)]).await.unwrap().dict_get("jobID").and_then(|v| v.as_i64()).unwrap();

    ctx.advance_secs(7_200);
    sync_all_structures(&ctx);
    let _ = rpc(&ctx, &mut sess, "industryManager", "GetJobsByOwner", &[PyRep::Integer(P1)]).await.unwrap();
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.industry_jobs[&j1].status, "Active");
        assert_eq!(snap.industry_jobs[&j_long].status, "Paused");
        assert_eq!(load_job_cost_meta(snap, j_long).unwrap().paused_at_unix, Some(base_u + 12 + 3_600));
    });

    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.get_mut(&struct_id).unwrap().fuel_blocks = 5_000;
        snap.items.get_mut(&(struct_id + 1)).unwrap().dynamic_attributes.insert(2, 1.0);
        snap.module_online_states.insert(struct_id + 1, true);
    });
    sync_all_structures(&ctx);
    let _ = rpc(&ctx, &mut sess, "industryManager", "GetJobsByOwner", &[PyRep::Integer(P1)]).await.unwrap();
    let many_rep = rpc(&ctx, &mut sess, "industryManager", "CompleteManyJobs", &[PyRep::List(vec![PyRep::Integer(j1), PyRep::Integer(j2)])]).await.unwrap();
    let many_str = format!("{many_rep:?}");
    assert!(many_str.contains(&j1.to_string()) && many_str.contains(&j2.to_string()));

    let j_corp = rpc(&ctx, &mut sess, "industryManager", "InstallJob", &[PyRep::Dict(vec![
        (PyRep::String("blueprintID".into()), PyRep::Integer(710_004)),
        (PyRep::String("activityID".into()), PyRep::Integer(1)),
        (PyRep::String("runs".into()), PyRep::Integer(1)),
        (PyRep::String("useCorp".into()), PyRep::Bool(true)),
        (PyRep::String("ownerID".into()), PyRep::Integer(CORP_ID)),
    ])]).await.unwrap().dict_get("jobID").and_then(|v| v.as_i64()).unwrap();

    // `owner_notes-persistence`: place order (duration 1), install corp job at station, build PI network via RPCs
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&P1) {
            ch.structure_id = None;
            ch.station_id = Some(JITA_STATION_ID);
        }
    });
    sess.structure_id = None;
    sess.station_id = Some(JITA_STATION_ID);
    let buy_res = rpc(&ctx, &mut sess, "marketProxy", "PlaceBuyOrder", &[
        PyRep::Integer(JITA_STATION_ID), PyRep::Integer(34), PyRep::Real(3.50),
        PyRep::Integer(1_000), PyRep::Integer(5), PyRep::Integer(10),
        PyRep::Integer(1), PyRep::Bool(true), PyRep::Integer(1002),
    ]).await.unwrap();
    let order_id = buy_res.dict_get("orderID").and_then(|v| v.as_i64()).unwrap();
    let corp_orders = rowset_maps(&rpc(&ctx, &mut sess, "marketProxy", "GetCorporationOrders", &[]).await.unwrap());
    let ord_row = corp_orders.iter().find(|r| r.get("orderID").and_then(|v| v.as_i64()) == Some(order_id)).unwrap();
    let issue_ft = ord_row.get("issueDate").and_then(|v| v.as_i64()).unwrap();
    let escrow_isk = ord_row.get("escrow").and_then(|v| v.as_f64()).unwrap();

    let j_corp_deliver = rpc_kw(
        &ctx, &mut sess, "industryManager", "InstallJob",
        &[PyRep::Integer(691), PyRep::Integer(1), PyRep::Integer(1)],
        &[("useCorp", PyRep::Bool(true))],
    ).await.unwrap().dict_get("jobID").and_then(|v| v.as_i64()).unwrap();

    let ship_id = sess.ship_id.unwrap();
    sess.station_id = None;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&P1) {
            ch.station_id = None;
        }
        snap.items.insert(710_030, make_item(710_030, 2534, P1, ship_id, InventoryFlag::Cargo, 1, false));
    });
    let res_info = rpc(&ctx, &mut sess, "planetMgr", "GetPlanetResourceInfo", &[PyRep::Integer(JITA_PLANET_ID)]).await.unwrap();
    let PyRep::List(res_list) = res_info.dict_get("resources").unwrap() else { panic!() };
    let p0_tid = res_list[0].dict_get("resourceTypeID").and_then(|v| v.as_i64()).unwrap();
    let pi_cmds = vec![
        PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(101), PyRep::Integer(2534), PyRep::Real(0.10), PyRep::Real(0.10)]),
        PyRep::Tuple(vec![PyRep::Integer(9), PyRep::Integer(101), PyRep::Integer(3)]),
        PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(102), PyRep::Integer(2848), PyRep::Real(0.11), PyRep::Real(0.11)]),
        PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(103), PyRep::Integer(2492), PyRep::Real(0.12), PyRep::Real(0.12)]),
        pi_cmd(3, 101, 102, 0),
        pi_cmd(3, 102, 103, 0),
        pi_cmd(5, 102, 103, 1),
        PyRep::Tuple(vec![PyRep::Integer(7), PyRep::Integer(103), PyRep::Integer(121)]),
        PyRep::Tuple(vec![PyRep::Integer(6), PyRep::Integer(501), PyRep::List(vec![PyRep::Integer(102), PyRep::Integer(103)]), PyRep::Integer(p0_tid), PyRep::Integer(3_000)]),
        PyRep::Tuple(vec![PyRep::Integer(10), PyRep::Integer(102), PyRep::Integer(p0_tid), PyRep::Real(0.02), PyRep::Integer(3)]),
    ];
    rpc(&ctx, &mut sess, "planetMgr", "UserUpdateNetwork", &[PyRep::Integer(JITA_PLANET_ID), PyRep::List(pi_cmds)]).await.unwrap();
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&P1) {
            ch.station_id = Some(JITA_STATION_ID);
        }
    });

    drop(ctx);
    let ctx2 = open_ctx_at(&db_path, base_u + 50_400);
    let mut sess2 = login_select(&ctx2, 9202, "demo", "demo-secret", P1).await;

    let corp_orders2 = rowset_maps(&rpc(&ctx2, &mut sess2, "marketProxy", "GetCorporationOrders", &[]).await.unwrap());
    let ord2 = corp_orders2.iter().find(|r| r.get("orderID").and_then(|v| v.as_i64()) == Some(order_id)).unwrap();
    assert_eq!(ord2.get("issueDate").and_then(|v| v.as_i64()), Some(issue_ft));
    assert_eq!(ord2.get("duration").and_then(|v| v.as_i64()), Some(1));
    assert_eq!(ord2.get("range").and_then(|v| v.as_i64()), Some(5));
    assert_eq!(ord2.get("accountID").and_then(|v| v.as_i64()), Some(1002));

    let div3_before = ctx2.db.with_snapshot(|s| s.corporations[&CORP_ID].wallet_divisions.iter().find(|d| d.division_id == 3).unwrap().balance.0);
    ctx2.advance_secs(43_201);
    let hist_rows = rowset_maps(&rpc_kw(&ctx2, &mut sess2, "marketProxy", "GetMarketOrderHistory", &[PyRep::Bool(true)], &[("isCorp", PyRep::Bool(true))]).await.unwrap());
    let exp_row = hist_rows.iter().find(|r| r.get("orderID").and_then(|v| v.as_i64()) == Some(order_id)).unwrap();
    assert_eq!(exp_row.get("orderState").and_then(|v| v.as_i64()), Some(4));
    let div3_after = ctx2.db.with_snapshot(|s| s.corporations[&CORP_ID].wallet_divisions.iter().find(|d| d.division_id == 3).unwrap().balance.0);
    assert_eq!(div3_after - div3_before, (escrow_isk * 100.0).round() as i64);

    let deliv = rpc(&ctx2, &mut sess2, "industryManager", "CompleteJob", &[PyRep::Integer(j_corp_deliver)]).await.unwrap();
    let deliv_id = deliv.dict_get("deliveredItemID").and_then(|v| v.as_i64()).unwrap();
    ctx2.db.with_snapshot(|s| {
        assert_eq!(s.items[&deliv_id].type_id, 587);
        assert_eq!(s.items[&deliv_id].owner_id, CORP_ID);
    });

    let net = rpc(&ctx2, &mut sess2, "planetMgr", "GetFullNetworkForOwner", &[PyRep::Integer(JITA_PLANET_ID), PyRep::Integer(P1)]).await.unwrap();
    let PyRep::List(pins) = net.dict_get("pins").unwrap() else { panic!() };
    let PyRep::List(links) = net.dict_get("links").unwrap() else { panic!() };
    let PyRep::List(routes) = net.dict_get("routes").unwrap() else { panic!() };
    assert_eq!((pins.len(), links.len(), routes.len()), (3, 2, 1));
    let ext_id = pins.iter().find(|p| p.dict_get("typeID").and_then(|v| v.as_i64()) == Some(2848)).unwrap().dict_get("pinID").and_then(|v| v.as_i64()).unwrap();
    let ext_pin = rpc(&ctx2, &mut sess2, "planetMgr", "GetPin", &[PyRep::Integer(JITA_PLANET_ID), PyRep::Integer(ext_id)]).await.unwrap();
    assert_eq!(ext_pin.dict_get("headCount").and_then(|v| v.as_i64()), Some(3));

    // Ex-member cannot cancel or deliver corp job
    ctx2.db.with_mut_snapshot(|snap| snap.characters.get_mut(&P1).unwrap().corporation_id = 9999);
    assert_eq!(rpc(&ctx2, &mut sess2, "industryManager", "CancelJob", &[PyRep::Integer(j_corp)]).await.unwrap_err().code, "JobOwnershipViolation");
    assert_eq!(rpc(&ctx2, &mut sess2, "industryManager", "CompleteJob", &[PyRep::Integer(j_corp)]).await.unwrap_err().code, "JobOwnershipViolation");
    let _ = std::fs::remove_file(&db_path);
}

#[tokio::test]
async fn test_b30_industry_pi_and_market_wallet_history_durable() {
    // parity: feat:lifecycle.industry-pi-durable, feat:lifecycle.market-wallet-history-durable
    eve_testkit::attest(&[
        "feat:lifecycle.industry-pi-durable",
        "feat:lifecycle.market-wallet-history-durable",
    ]);

    let db_path = temp_db_path("pi_wallet");
    let base_u = 1_800_000_000u64;
    let ctx = open_fresh_ctx_at(&db_path, base_u);
    let athanor_id = 1_045_000_000_101i64;
    let metenox_id = 1_045_000_000_102i64;

    ctx.db.with_mut_snapshot(|snap| {
        seed_corp_full(snap, P1, CORP_ID);
        snap.characters.get_mut(&P1).unwrap().standings.insert(1_000_084, 6.0);
        snap.characters.get_mut(&P2).unwrap().balance = IskCents::from_isk(500_000_000);
        for &(tid, lvl) in &[(2403, 5), (2406, 5), (2495, 5), (2505, 5), (3380, 5), (3386, 5), (3402, 5), (11448, 4), (13279, 5), (46152, 5)] {
            snap.skills.entry(P1).or_default().insert(tid, TrainedSkillRecord { type_id: tid, level: lvl, skill_points: 256_000.0 });
        }
        for (iid, tid, qty, sing) in [(910_002, 691u32, 1, true), (910_003, 34u32, 5_000_000, false), (910_004, 35u32, 1_000_000, false), (910_008, 52_568u32, 500, false), (910_009, 587u32, 1, false)] {
            snap.items.insert(iid, make_item(iid, tid, P1, JITA_STATION_ID, InventoryFlag::Hangar, qty, sing));
        }
        snap.structures.insert(athanor_id, UpwellStructureRecord {
            structure_id: athanor_id, type_id: 35835, owner_corp_id: CORP_ID, solar_system_id: JITA_SYSTEM_ID,
            name: "B30 Athanor".into(), position: [0.0, 0.0, 0.0], state: "ShieldVulnerable".into(),
            quantum_core_installed: true, fuel_blocks: 500, shield_hp: 10000.0, armor_hp: 10000.0, hull_hp: 10000.0,
            reinforced_until_unix: 0, last_damage_tick: 0, last_damage_unix: 0, self_destruct_at_unix: None,
        });
        snap.structures.insert(metenox_id, UpwellStructureRecord {
            structure_id: metenox_id, type_id: 81826, owner_corp_id: CORP_ID, solar_system_id: 30_004_608,
            name: "B30 Metenox".into(), position: [1000.0, 0.0, 0.0], state: "ShieldVulnerable".into(),
            quantum_core_installed: true, fuel_blocks: 200, shield_hp: 10000.0, armor_hp: 10000.0, hull_hp: 10000.0,
            reinforced_until_unix: 0, last_damage_tick: 0, last_damage_unix: 0, self_destruct_at_unix: None,
        });
        snap.items.insert(910_020, make_item(910_020, 81143, CORP_ID, metenox_id, InventoryFlag::Hangar, 500, false));
        save_orbital_settings(snap, JITA_PLANET_ID, &OrbitalCustomsSettings { planet_id: JITA_PLANET_ID, owner_corp_id: CORP_ID, ..Default::default() });
    });

    let mut s1 = login_select(&ctx, 9601, "demo", "demo-secret", P1).await;
    let mut s2 = login_select(&ctx, 9602, "other", "other-secret", P2).await;

    rpc(&ctx, &mut s1, "moonExtractions", "StartNewExtraction", &[PyRep::Integer(athanor_id), PyRep::Integer(518_400)]).await.unwrap();
    ctx.advance_secs(511_200);
    ctx.db.with_mut_snapshot(|s| { s.structures.get_mut(&metenox_id).unwrap().fuel_blocks = 200; s.items.get_mut(&910_020).unwrap().quantity = 500; s.auto_moon_miners.remove(&metenox_id); });
    ctx.advance_secs(7_200);
    rpc(&ctx, &mut s1, "autoMoonMining", "RequestMiningCycleOutput", &[PyRep::Integer(metenox_id)]).await.unwrap();
    rpc_kw(&ctx, &mut s1, "planetOrbitalRegistryBroker", "UpdateSettings", &[PyRep::Integer(JITA_PLANET_ID)], &[("taxRateValues", PyRep::keyval([("corporation", PyRep::Real(0.075))]))]).await.unwrap();

    let rnd = ctx.sde.agents.values().filter(|a| a.agent_type_id == 4 && a.station_id.is_some()).min_by_key(|a| a.agent_id).cloned().unwrap();
    ctx.db.with_mut_snapshot(|s| { if let Some(ch) = s.characters.get_mut(&P1) { ch.standings.insert(rnd.agent_id, 9.0); ch.solar_system_id = rnd.solar_system_id.unwrap(); ch.station_id = rnd.station_id; } });
    s1.solar_system_id = rnd.solar_system_id;
    s1.station_id = rnd.station_id;
    rpc(&ctx, &mut s1, "agentMgr", "DoAction", &[PyRep::Integer(12), PyRep::Integer(rnd.agent_id), PyRep::Integer(11448)]).await.unwrap();
    s1.solar_system_id = Some(JITA_SYSTEM_ID);
    s1.station_id = Some(JITA_STATION_ID);
    ctx.db.with_mut_snapshot(|s| { if let Some(ch) = s.characters.get_mut(&P1) { ch.solar_system_id = JITA_SYSTEM_ID; ch.station_id = Some(JITA_STATION_ID); } });
    let PyRep::Tuple(jp) = rpc(&ctx, &mut s1, "agentMgr", "GetMyJournalDetails", &[]).await.unwrap() else { panic!() };
    let PyRep::List(rl) = &jp[1] else { panic!() };
    let rnd_rp_before = rl[0].dict_get("points").and_then(|v| v.as_f64()).unwrap_or(0.0);

    let job_id = rpc(&ctx, &mut s1, "industryManager", "InstallJob", &[PyRep::Integer(691), PyRep::Integer(1), PyRep::Integer(2)]).await.unwrap().dict_get("jobID").and_then(|v| v.as_i64()).unwrap();
    let sell_oid = rpc(&ctx, &mut s1, "marketProxy", "PlaceMultiSellOrder", &[PyRep::List(vec![PyRep::Dict(vec![
        (PyRep::String("itemID".into()), PyRep::Integer(910_003)), (PyRep::String("typeID".into()), PyRep::Integer(34)),
        (PyRep::String("price".into()), PyRep::Real(4.50)), (PyRep::String("quantity".into()), PyRep::Integer(1_000)),
        (PyRep::String("stationID".into()), PyRep::Integer(JITA_STATION_ID)), (PyRep::String("duration".into()), PyRep::Integer(90)),
    ])]), PyRep::Bool(false)]).await.unwrap().dict_get("orderID").and_then(|v| v.as_i64()).unwrap();
    rpc(&ctx, &mut s2, "marketProxy", "PlaceBuyOrder", &[PyRep::Integer(JITA_STATION_ID), PyRep::Integer(34), PyRep::Real(4.50), PyRep::Integer(400), PyRep::Integer(-1), PyRep::Integer(1), PyRep::Integer(90), PyRep::Bool(false)]).await.unwrap();
    let old_hist_before = rpc(&ctx, &mut s1, "marketProxy", "GetOldPriceHistory", &[PyRep::Integer(34)]).await.unwrap();
    rpc_kw(&ctx, &mut s1, "account", "GiveCashFromCorpAccount", &[PyRep::Integer(P2), PyRep::Real(50_000.0), PyRep::Integer(1000)], &[("reason", PyRep::String("B30 dividend".into()))]).await.unwrap();
    let bill_id = create_bill(&ctx, 2, CORP_ID, 1_000_035, 250_000.0, ctx.now_unix() + 86_400, 0.0, JITA_STATION_ID, 0);
    let trade_id = rpc(&ctx, &mut s1, "trademgr", "InitiateTrade", &[PyRep::Integer(P2)]).await.unwrap().dict_get("tradeContainerID").and_then(|v| v.as_i64()).unwrap();
    rpc(&ctx, &mut s1, "trademgr", "OfferMoney", &[PyRep::Integer(trade_id), PyRep::Real(125_000.0)]).await.unwrap();
    let raffle_id = rpc(&ctx, &mut s1, "raffleProxy", "CreateRaffle", &[PyRep::Dict(vec![
        (PyRep::String("item_id".into()), PyRep::Integer(910_009)), (PyRep::String("ticket_count".into()), PyRep::Integer(8)), (PyRep::String("ticket_price".into()), PyRep::Real(100_000.0)),
    ])]).await.unwrap().dict_get("raffle_id").and_then(|v| v.as_i64()).unwrap();
    let frac = rpc(&ctx, &mut s1, "moonExtractions", "FractureChunkWithStructure", &[PyRep::Integer(athanor_id)]).await.unwrap();
    let PyRep::List(ast_ids) = frac.dict_get("asteroidIDs").unwrap() else { panic!() };
    let first_ast_id = ast_ids[0].as_i64().unwrap();
    let undock = rpc(&ctx, &mut s1, "stationSvc", "Undock", &[]).await.unwrap();
    let m_ship_id = undock.dict_get("shipID").and_then(|v| v.as_i64()).unwrap();
    ctx.db.with_mut_snapshot(|s| {
        if let Some(sh) = s.items.get_mut(&m_ship_id) { sh.type_id = 32880; }
        s.items.retain(|_, it| it.location_id != m_ship_id);
        s.items.insert(910_030, make_item(910_030, 483, P1, m_ship_id, InventoryFlag::HiSlot0, 1, true));
    });
    ctx.invalidate_ship_fitting(m_ship_id);
    let jita = ctx.systems.get_or_spawn(JITA_SYSTEM_ID);
    let undock_pos = jita.get_ball_snapshot(m_ship_id).await.unwrap().unwrap().1.position;
    let ast_pos = jita.get_ball_snapshot(first_ast_id).await.unwrap().unwrap().1.position;
    if let Ok(Some((_, mut sb))) = jita.get_ball_snapshot(m_ship_id).await { sb.position = ast_pos + glam::DVec3::new(500.0, 0.0, 0.0); let _ = jita.spawn_ball(sb).await; }
    let _ = crate::mining_cycle_engine::mine_once(&ctx, &mut s1, 910_030, first_ast_id).await.unwrap();
    if let Ok(Some((_, mut sb))) = jita.get_ball_snapshot(m_ship_id).await { sb.position = undock_pos; let _ = jita.spawn_ball(sb).await; }
    rpc(&ctx, &mut s1, "stationSvc", "Dock", &[PyRep::Integer(JITA_STATION_ID)]).await.unwrap();

    let saved_u = ctx.now_unix();
    let (wj_before, wt_before) = ctx.db.with_snapshot(|s| (s.wallet_journal.len(), s.char_wallet_transactions.len()));
    drop(ctx);

    let ctx2 = open_ctx_at(&db_path, saved_u + 3_600);
    let mut s1_re = login_select(&ctx2, 9603, "demo", "demo-secret", P1).await;
    assert_eq!(rpc(&ctx2, &mut s1_re, "planetOrbitalRegistryBroker", "GetTaxRate", &[PyRep::Integer(JITA_PLANET_ID)]).await.unwrap().as_f64(), Some(0.075));
    let PyRep::Tuple(ext_tup) = rpc(&ctx2, &mut s1_re, "moonExtractions", "GetExtractionAndEventIDFromStructureID", &[PyRep::Integer(athanor_id)]).await.unwrap() else { panic!() };
    assert!(ext_tup[0].dict_get("status").is_some());
    let met_det = rpc(&ctx2, &mut s1_re, "autoMoonMining", "RequestMiningDetails", &[PyRep::Integer(metenox_id)]).await.unwrap();
    assert_eq!(met_det.dict_get("status").and_then(|v| v.as_str()), Some("Active"));
    let obs = rpc(&ctx2, &mut s1_re, "corpMiningLedger", "GetObserverLedger", &[PyRep::Integer(athanor_id)]).await.unwrap();
    assert!(format!("{obs:?}").contains(&P1.to_string()));
    let PyRep::Tuple(jp2) = rpc(&ctx2, &mut s1_re, "agentMgr", "GetMyJournalDetails", &[]).await.unwrap() else { panic!() };
    let PyRep::List(rl2) = &jp2[1] else { panic!() };
    assert!(rl2[0].dict_get("points").and_then(|v| v.as_f64()).unwrap_or(0.0) > rnd_rp_before);
    assert!(format!("{:?}", rpc(&ctx2, &mut s1_re, "industryManager", "GetJobsByOwner", &[PyRep::Integer(P1)]).await.unwrap()).contains(&job_id.to_string()));

    assert_eq!(rpc(&ctx2, &mut s1_re, "marketProxy", "GetOldPriceHistory", &[PyRep::Integer(34)]).await.unwrap(), old_hist_before);
    assert!(format!("{:?}", rpc(&ctx2, &mut s1_re, "marketProxy", "GetCharOrders", &[]).await.unwrap()).contains(&sell_oid.to_string()));
    assert!(format!("{:?}", rpc(&ctx2, &mut s1_re, "billMgr", "GetCorporationBills", &[]).await.unwrap()).contains(&bill_id.to_string()));
    assert_eq!(rpc(&ctx2, &mut s1_re, "trademgr", "List", &[PyRep::Integer(trade_id)]).await.unwrap().dict_get("myMoney").and_then(|v| v.as_f64()), Some(125_000.0));
    assert!(format!("{:?}", rpc(&ctx2, &mut s1_re, "raffleProxy", "GetRaffle", &[PyRep::Integer(raffle_id)]).await.unwrap()).contains(&raffle_id.to_string()));
    ctx2.db.with_snapshot(|s| {
        assert_eq!(s.wallet_journal.len(), wj_before);
        assert_eq!(s.char_wallet_transactions.len(), wt_before);
    });
    let _ = std::fs::remove_file(&db_path);
}
