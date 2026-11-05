//! End-to-end integration tests for `feat:lifecycle.*` parity rows (Batch 30, Part 1).
//!
//! Covers:
//! - `feat:lifecycle.fleet-kick-session`, `feat:lifecycle.fleet-login-consistency`,
//!   `feat:lifecycle.fleet-logout`, `feat:lifecycle.fleet-server-restart`
//! - `feat:lifecycle.owner_notes-persistence`
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
    // parity: feat:lifecycle.owner_notes-persistence
    eve_testkit::attest(&[
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

    // Build PI colony network via UserUpdateNetwork for durable PI verification
    let ship_pi = s1.ship_id.unwrap();
    s1.station_id = None;
    ctx.db.with_mut_snapshot(|s| {
        if let Some(ch) = s.characters.get_mut(&P1) { ch.station_id = None; }
        s.items.insert(910_040, make_item(910_040, 2534, P1, ship_pi, InventoryFlag::Cargo, 1, false));
    });
    rpc(&ctx, &mut s1, "planetMgr", "UserUpdateNetwork", &[PyRep::Integer(JITA_PLANET_ID), PyRep::List(vec![
        PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(201), PyRep::Integer(2534), PyRep::Real(0.20), PyRep::Real(0.20)]),
        PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(202), PyRep::Integer(2848), PyRep::Real(0.22), PyRep::Real(0.22)]),
        pi_cmd(3, 201, 202, 0),
    ])]).await.unwrap();
    s1.station_id = Some(JITA_STATION_ID);
    ctx.db.with_mut_snapshot(|s| { if let Some(ch) = s.characters.get_mut(&P1) { ch.station_id = Some(JITA_STATION_ID); } });

    let rnd = ctx.sde.agents.values().filter(|a| a.agent_type_id == 4 && a.station_id.is_some()).min_by_key(|a| a.agent_id).cloned().unwrap();
    ctx.db.with_mut_snapshot(|s| {
        if let Some(ch) = s.characters.get_mut(&P1) { ch.standings.insert(rnd.agent_id, 9.0); ch.solar_system_id = rnd.solar_system_id.unwrap(); ch.station_id = rnd.station_id; }
        let sk = s.skills.entry(P1).or_default();
        for sid in [3402_u32, 3392_u32, 11448_u32] {
            sk.insert(sid, eve_db::TrainedSkillRecord { type_id: sid, level: 5, skill_points: 256_000.0 });
        }
    });
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
    let char_orders_before = rowset_maps(&rpc(&ctx, &mut s1, "marketProxy", "GetCharOrders", &[]).await.unwrap());
    let sell_row_b = char_orders_before.iter().find(|r| r.get("orderID").and_then(|v| v.as_i64()) == Some(sell_oid)).unwrap();
    let sell_issue_ft = sell_row_b.get("issueDate").and_then(|v| v.as_i64()).unwrap();
    let old_hist_before = rpc(&ctx, &mut s1, "marketProxy", "GetOldPriceHistory", &[PyRep::Integer(34)]).await.unwrap();
    let char_tx_before = rpc(&ctx, &mut s1, "marketProxy", "CharGetTransactions", &[]).await.unwrap();
    let corp_tx_before = rpc(&ctx, &mut s1, "marketProxy", "CorpGetTransactions", &[PyRep::Integer(1000)]).await.unwrap();
    let acct_tx_before = rpc(&ctx, &mut s1, "account", "GetTransactions", &[PyRep::Integer(1000)]).await.unwrap();
    rpc_kw(&ctx, &mut s1, "account", "GiveCashFromCorpAccount", &[PyRep::Integer(P2), PyRep::Real(50_000.0), PyRep::Integer(1000)], &[("reason", PyRep::String("B30 dividend".into()))]).await.unwrap();
    let bill_id = create_bill(&ctx, 2, CORP_ID, 1_000_035, 250_000.0, ctx.now_unix() + 86_400, 0.0, JITA_STATION_ID, 0);
    rpc(&ctx, &mut s1, "billMgr", "SendAutomaticPaySettings", &[PyRep::Dict(vec![(PyRep::Integer(2), PyRep::Tuple(vec![PyRep::Bool(true), PyRep::Integer(1000)]))])]).await.unwrap();
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
    let pi_net = rpc(&ctx2, &mut s1_re, "planetMgr", "GetFullNetworkForOwner", &[PyRep::Integer(JITA_PLANET_ID), PyRep::Integer(P1)]).await.unwrap();
    let PyRep::List(pi_pins) = pi_net.dict_get("pins").unwrap() else { panic!() };
    assert_eq!(pi_pins.len(), 2);
    assert_eq!(pi_net.dict_get("links").and_then(|v| match v { PyRep::List(l) => Some(l.len()), _ => None }), Some(1));
    let pin0_id = pi_pins[0].dict_get("pinID").and_then(|v| v.as_i64()).unwrap();
    assert!(rpc(&ctx2, &mut s1_re, "planetMgr", "GetPin", &[PyRep::Integer(JITA_PLANET_ID), PyRep::Integer(pin0_id)]).await.is_ok());

    let PyRep::Tuple(ext_tup) = rpc(&ctx2, &mut s1_re, "moonExtractions", "GetExtractionAndEventIDFromStructureID", &[PyRep::Integer(athanor_id)]).await.unwrap() else { panic!() };
    assert!(ext_tup[0].dict_get("status").is_some());
    let met_det = rpc(&ctx2, &mut s1_re, "autoMoonMining", "RequestMiningDetails", &[PyRep::Integer(metenox_id)]).await.unwrap();
    assert_eq!(met_det.dict_get("status").and_then(|v| v.as_str()), Some("Active"));
    let obs = rpc(&ctx2, &mut s1_re, "corpMiningLedger", "GetObserverLedger", &[PyRep::Integer(athanor_id)]).await.unwrap();
    assert!(format!("{obs:?}").contains(&P1.to_string()));
    let PyRep::Tuple(jp2) = rpc(&ctx2, &mut s1_re, "agentMgr", "GetMyJournalDetails", &[]).await.unwrap() else { panic!() };
    let PyRep::List(rl2) = &jp2[1] else { panic!() };
    assert!(rl2[0].dict_get("points").and_then(|v| v.as_f64()).unwrap_or(0.0) > rnd_rp_before);
    let jobs_rep = rpc(&ctx2, &mut s1_re, "industryManager", "GetJobsByOwner", &[PyRep::Integer(P1)]).await.unwrap();
    assert!(format!("{jobs_rep:?}").contains(&job_id.to_string()));
    let PyRep::List(job_list) = &jobs_rep else { panic!() };
    let j_row = job_list.iter().find(|r| r.dict_get("jobID").and_then(|v| v.as_i64()) == Some(job_id)).unwrap();
    assert!(j_row.dict_get("facilityTaxISK").and_then(|v| v.as_f64()).unwrap_or(0.0) > 0.0);
    assert!(j_row.dict_get("totalJobCostISK").and_then(|v| v.as_f64()).unwrap_or(0.0) > 0.0);

    assert_eq!(rpc(&ctx2, &mut s1_re, "marketProxy", "GetOldPriceHistory", &[PyRep::Integer(34)]).await.unwrap(), old_hist_before);
    let char_orders_after = rowset_maps(&rpc(&ctx2, &mut s1_re, "marketProxy", "GetCharOrders", &[]).await.unwrap());
    let sell_row_a = char_orders_after.iter().find(|r| r.get("orderID").and_then(|v| v.as_i64()) == Some(sell_oid)).unwrap();
    assert_eq!(sell_row_a.get("issueDate").and_then(|v| v.as_i64()), Some(sell_issue_ft));
    assert_eq!(sell_row_a.get("duration").and_then(|v| v.as_i64()), Some(90));
    assert_eq!(sell_row_a.get("accountID").and_then(|v| v.as_i64()), Some(1000));
    assert_eq!(rpc(&ctx2, &mut s1_re, "marketProxy", "CharGetTransactions", &[]).await.unwrap(), char_tx_before);
    assert_eq!(rpc(&ctx2, &mut s1_re, "marketProxy", "CorpGetTransactions", &[PyRep::Integer(1000)]).await.unwrap(), corp_tx_before);
    assert_eq!(rpc(&ctx2, &mut s1_re, "account", "GetTransactions", &[PyRep::Integer(1000)]).await.unwrap(), acct_tx_before);
    assert!(format!("{:?}", rpc(&ctx2, &mut s1_re, "billMgr", "GetCorporationBills", &[]).await.unwrap()).contains(&bill_id.to_string()));
    assert!(rpc(&ctx2, &mut s1_re, "billMgr", "GetAutomaticPaySettings", &[]).await.is_ok());
    assert_eq!(rpc(&ctx2, &mut s1_re, "trademgr", "List", &[PyRep::Integer(trade_id)]).await.unwrap().dict_get("myMoney").and_then(|v| v.as_f64()), Some(125_000.0));
    assert!(format!("{:?}", rpc(&ctx2, &mut s1_re, "raffleProxy", "GetRaffle", &[PyRep::Integer(raffle_id)]).await.unwrap()).contains(&raffle_id.to_string()));
    ctx2.db.with_snapshot(|s| {
        assert_eq!(s.wallet_journal.len(), wj_before);
        assert_eq!(s.char_wallet_transactions.len(), wt_before);
    });
    let _ = std::fs::remove_file(&db_path);
}

#[allow(unused_imports, dead_code, unused_variables, unused_mut)]
pub(crate) mod part2 {
    use super::*;

// End-to-end integration tests for `feat:lifecycle.*` parity rows (Batch 30, Part 2).
//
// Covers:
// - `feat:lifecycle.mail-no-global-cap`, `feat:lifecycle.mail-state-persist`,
//   `feat:lifecycle.notifications-durable`
// - `feat:lifecycle.market_order-expiry-trigger`, `feat:lifecycle.lp-durable`
// - `feat:lifecycle.jetcan-empty`, `feat:lifecycle.mobile_warp_disruptor-lifecycle`,
//   `feat:lifecycle.module-cycles-stop-on-leave-space`, `feat:lifecycle.mission-downtime-respawn`


use crate::common;
use eve_db::{
    corp_roles, CorporationRecord, InventoryFlag, InventoryItemRecord,
    IskCents, MailRecord, MarketOrderRecord, OwnerNoteRecord, TrainedSkillRecord,
    VirtualServerClock, WorldRepository,
};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use eve_server::services::combat_crimewatch::with_combat_runtime;
use eve_server::services::wallet_serverops::run_daily_downtime_and_jobs;
use glam::DVec3;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

const P1: i64 = 90_000_001;
const P2: i64 = 90_000_002;
const CORP_ID: i64 = 2001;
const JITA_SYSTEM_ID: u32 = 30_000_142;
const JITA_STATION_ID: i64 = 60_003_760;
const AMARR_SYSTEM_ID: u32 = 30_002_187;
const AMARR_REGION_ID: u32 = 10_000_043;
const AMARR_STATION_ID: i64 = 60_008_494;

fn temp_db_path(tag: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!("evomu_b30_p2_{tag}_{}.sqlite", std::process::id()));
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

async fn login_select(ctx: &ServerContext, sid: u64, user: &str, pass: &str, char_id: i64) -> SessionState {
    let mut sess = SessionState::new(sid);
    common::login_pilot(ctx, &mut sess, user, pass).await;
    rpc(ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(char_id)]).await.expect("SelectCharacterID");
    ctx.advance_secs(12);
    sess
}

fn make_item(item_id: i64, type_id: u32, owner_id: i64, location_id: i64, flag: InventoryFlag, quantity: i32, singleton: bool) -> InventoryItemRecord {
    InventoryItemRecord { item_id, type_id, owner_id, location_id, flag, quantity, singleton, version: 1, ..Default::default() }
}

fn insert_expired_order(snap: &mut eve_db::WorldStateSnapshot, oid: i64, owner: i64, tid: u32, is_bid: bool, price: IskCents, qty: i32, escrow: IskCents, now_u: u64) {
    snap.market_orders.insert(oid, MarketOrderRecord {
        order_id: oid, owner_id: owner, station_id: AMARR_STATION_ID, region_id: AMARR_REGION_ID,
        type_id: tid, is_bid, price, vol_entered: qty, vol_remaining: qty, min_volume: 1, is_npc: false, escrow,
    });
    let nid = -(4_000_000_000_000_i64.saturating_add(oid.abs()));
    snap.owner_notes.insert(nid, OwnerNoteRecord {
        note_id: nid, character_id: -4, item_id: oid, label: format!("MarketOrderMeta:{oid}"),
        note: format!("{{\"range\":32767,\"duration\":1,\"issue_unix\":{}}}", now_u.saturating_sub(86_500)), timestamp_unix: now_u,
    });
}

#[tokio::test]
async fn test_b30_mail_no_global_cap_state_and_notifications_durability() {
    // parity: feat:lifecycle.mail-no-global-cap, feat:lifecycle.mail-state-persist, feat:lifecycle.notifications-durable
    eve_testkit::attest(&[
        "feat:lifecycle.mail-no-global-cap",
        "feat:lifecycle.mail-state-persist",
        "feat:lifecycle.notifications-durable",
    ]);

    let db_path = temp_db_path("mail_notif");
    let base_u = 1_760_000_000u64;
    let ctx = open_fresh_ctx_at(&db_path, base_u);

    ctx.db.with_mut_snapshot(|snap| {
        for i in 1..=3_001i64 {
            snap.mails.push(MailRecord {
                message_id: 100_000 + i, sender_id: P1, recipient_id: 80_000_000 + (i % 50),
                subject: format!("Bulk {i}"), body: "Body".into(),
            });
        }
    });

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&P2) {
            ch.corporation_id = CORP_ID;
        }
        if let Some(c) = snap.corporations.get_mut(&CORP_ID) {
            c.members.insert(P2);
            c.alliance_id = Some(99_000_001);
        }
        snap.alliances.insert(99_000_001, eve_db::AllianceRecord {
            alliance_id: 99_000_001, name: "B30 Alliance".into(), short_name: "B30A".into(),
            executor_corp_id: CORP_ID, member_corp_ids: [CORP_ID].into_iter().collect(), ..Default::default()
        });
    });

    let mut s1 = login_select(&ctx, 9301, "demo", "demo-secret", P1).await;
    s1.corporation_id = Some(CORP_ID);
    s1.alliance_id = Some(99_000_001);
    let ml_rep = rpc(&ctx, &mut s1, "mailingListsMgr", "Create", &[PyRep::String("B30 Dispatch".into()), PyRep::Integer(1), PyRep::Integer(0), PyRep::Integer(250)]).await.unwrap();
    let list_id = ml_rep.as_i64().unwrap();
    rpc(&ctx, &mut s1, "mailingListsMgr", "SaveWelcomeMail", &[PyRep::Integer(list_id), PyRep::String("Welcome".into()), PyRep::String("Body".into())]).await.unwrap();

    let mail_3002 = rpc(&ctx, &mut s1, "mailMgr", "SendMail", &[
        PyRep::List(vec![PyRep::Integer(P2)]), PyRep::None, PyRep::None,
        PyRep::String("Mail 3002".into()), PyRep::String("Delivered past 3001".into()),
        PyRep::Integer(0), PyRep::Integer(0),
    ]).await.unwrap().as_i64().unwrap();
    let mail_corp = rpc(&ctx, &mut s1, "mailMgr", "SendMail", &[
        PyRep::List(vec![PyRep::Integer(P2)]), PyRep::None, PyRep::Integer(CORP_ID),
        PyRep::String("Corp Mail".into()), PyRep::String("Corp body".into()),
        PyRep::Integer(0), PyRep::Integer(0),
    ]).await.unwrap().as_i64().unwrap();
    let mail_ally = rpc(&ctx, &mut s1, "mailMgr", "SendMail", &[
        PyRep::List(vec![PyRep::Integer(P2)]), PyRep::None, PyRep::Integer(99_000_001),
        PyRep::String("Ally Mail".into()), PyRep::String("Ally body".into()),
        PyRep::Integer(0), PyRep::Integer(0),
    ]).await.unwrap().as_i64().unwrap();
    let mail_trash = rpc(&ctx, &mut s1, "mailMgr", "SendMail", &[
        PyRep::List(vec![PyRep::Integer(P2)]), PyRep::None, PyRep::None,
        PyRep::String("Trash Mail".into()), PyRep::String("Trash".into()),
        PyRep::Integer(0), PyRep::Integer(0),
    ]).await.unwrap().as_i64().unwrap();
    let mail_del = rpc(&ctx, &mut s1, "mailMgr", "SendMail", &[
        PyRep::List(vec![PyRep::Integer(P2)]), PyRep::None, PyRep::None,
        PyRep::String("Del Mail".into()), PyRep::String("Del".into()),
        PyRep::Integer(0), PyRep::Integer(0),
    ]).await.unwrap().as_i64().unwrap();

    let notif_p1 = ctx.create_character_notification(1_000_125, P1, 10, 1, "B30 P1 processed notif").notification_id;
    rpc(&ctx, &mut s1, "notificationMgr", "MarkAsProcessed", &[PyRep::List(vec![PyRep::Integer(notif_p1)])]).await.unwrap();
    let notif_offline = ctx.create_character_notification(1_000_125, P2, 14, 2, "B30 offline notif").notification_id;

    let mut s2 = login_select(&ctx, 9302, "other", "other-secret", P2).await;
    let sync_rep = rpc(&ctx, &mut s2, "mailMgr", "SyncMail", &[PyRep::Integer(0), PyRep::Integer(0)]).await.unwrap();
    assert!(format!("{sync_rep:?}").contains(&mail_3002.to_string()));
    rpc(&ctx, &mut s2, "mailingListsMgr", "Join", &[PyRep::String("B30 Dispatch".into())]).await.unwrap();
    rpc(&ctx, &mut s1, "mailingListsMgr", "SetMembersOperator", &[PyRep::Integer(list_id), PyRep::List(vec![PyRep::Integer(P2)])]).await.unwrap();
    let lbl_id = rpc(&ctx, &mut s2, "mailMgr", "CreateLabel", &[PyRep::String("B30 Intel".into()), PyRep::Integer(4)]).await.unwrap().as_i64().unwrap_or(64);
    rpc(&ctx, &mut s2, "mailMgr", "AssignLabels", &[PyRep::List(vec![PyRep::Integer(mail_3002)]), PyRep::List(vec![PyRep::Integer(4), PyRep::Integer(lbl_id)])]).await.unwrap();
    rpc(&ctx, &mut s2, "mailMgr", "MarkAsRead", &[PyRep::List(vec![PyRep::Integer(mail_3002)])]).await.unwrap();
    rpc(&ctx, &mut s2, "mailMgr", "MoveToTrash", &[PyRep::List(vec![PyRep::Integer(mail_trash)])]).await.unwrap();
    rpc(&ctx, &mut s2, "mailMgr", "DeleteMail", &[PyRep::List(vec![PyRep::Integer(mail_del)])]).await.unwrap();
    rpc(&ctx, &mut s1, "calendarMgr", "CreatePersonalEvent", &[
        PyRep::Integer(135_500_000_000_000_000), PyRep::Integer(60),
        PyRep::String("B30 Formup".into()), PyRep::String("Ops".into()), PyRep::Integer(1), PyRep::List(vec![PyRep::Integer(P2)]),
    ]).await.unwrap();

    let (read_b, trash_b, del_b, lbl_b, ml_b, cal_b) = ctx.db.with_snapshot(|s| (
        s.read_mails.clone(), s.trashed_mails.clone(), s.deleted_mails.clone(),
        s.mail_assigned_labels.clone(), s.mailing_lists.clone(), s.calendar_events.clone(),
    ));
    drop(ctx);

    let ctx2 = open_ctx_at(&db_path, base_u + 100);
    let mut s1_re = login_select(&ctx2, 9303, "demo", "demo-secret", P1).await;
    let p1_notifs = rpc(&ctx2, &mut s1_re, "notificationMgr", "GetAllNotifications", &[]).await.unwrap();
    assert!(p1_notifs.crowset_rows().unwrap().iter().any(|r| r.dbrow_get("notificationID") == Some(&PyRep::Integer(notif_p1)) && r.dbrow_get("processed") == Some(&PyRep::Bool(true))));

    let mut s2_re = login_select(&ctx2, 9304, "other", "other-secret", P2).await;
    s2_re.corporation_id = Some(CORP_ID);
    s2_re.alliance_id = Some(99_000_001);
    let p2_login_notifs = ctx2.drain_notifications_for_char(P2);
    assert!(p2_login_notifs.iter().any(|n| n.event == "OnNotificationReceived" && n.args.first() == Some(&PyRep::Integer(notif_offline))));
    let p2_unproc = rpc(&ctx2, &mut s2_re, "notificationMgr", "GetUnprocessed", &[]).await.unwrap();
    assert!(p2_unproc.crowset_rows().unwrap().iter().any(|r| r.dbrow_get("notificationID") == Some(&PyRep::Integer(notif_offline))));
    let notif_after = ctx2.create_character_notification(1_000_125, P2, 14, 2, "B30 post-restart").notification_id;
    assert!(notif_after > notif_offline);

    // Verify mail headers, labels, mailing lists, and calendar events via RPCs post-restart
    let hdrs_s = format!("{:?}", rpc(&ctx2, &mut s2_re, "mailMgr", "GetMailHeaders", &[PyRep::List(vec![PyRep::Integer(mail_3002), PyRep::Integer(mail_corp), PyRep::Integer(mail_ally)])]).await.unwrap());
    assert!(hdrs_s.contains(&mail_3002.to_string()) && hdrs_s.contains(&mail_corp.to_string()) && hdrs_s.contains(&mail_ally.to_string()));
    assert!(format!("{:?}", rpc(&ctx2, &mut s2_re, "mailMgr", "GetLabels", &[]).await.unwrap()).contains("B30 Intel"));
    assert!(format!("{:?}", rpc(&ctx2, &mut s2_re, "mailingListsMgr", "GetJoinedLists", &[]).await.unwrap()).contains(&list_id.to_string()));
    assert!(rpc(&ctx2, &mut s2_re, "mailingListsMgr", "GetWelcomeMail", &[PyRep::Integer(list_id)]).await.is_ok());
    assert!(format!("{:?}", rpc(&ctx2, &mut s2_re, "calendarMgr", "GetEventList", &[PyRep::Integer(5), PyRep::Integer(2030)]).await.unwrap()).contains("B30 Formup"));

    ctx2.db.with_snapshot(|s| {
        assert_eq!(s.read_mails, read_b);
        assert_eq!(s.trashed_mails, trash_b);
        assert_eq!(s.deleted_mails, del_b);
        assert_eq!(s.mail_assigned_labels, lbl_b);
        assert_eq!(s.mailing_lists.len(), ml_b.len());
        assert_eq!(s.calendar_events.len(), cal_b.len());
    });
    let _ = std::fs::remove_file(&db_path);
}

#[tokio::test]
async fn test_b30_market_order_expiry_and_lp_durability() {
    // parity: feat:lifecycle.market_order-expiry-trigger, feat:lifecycle.lp-durable
    eve_testkit::attest(&[
        "feat:lifecycle.market_order-expiry-trigger",
        "feat:lifecycle.lp-durable",
    ]);

    let db_path = temp_db_path("market_lp");
    let base_u = 1_760_000_000u64;
    let ctx = open_fresh_ctx_at(&db_path, base_u);
    let (p2_bid, p2_ask) = (910_004i64, 910_005i64);

    ctx.db.with_mut_snapshot(|snap| {
        snap.market_orders.retain(|_, o| o.region_id != AMARR_REGION_ID);
        let roles = corp_roles::ROLE_DIRECTOR | corp_roles::ROLE_ACCOUNTANT | corp_roles::ROLE_TRADER;
        let mut corp = CorporationRecord {
            corporation_id: CORP_ID, name: "B30 Market Corp".into(), ticker: "B30M".into(), ceo_id: P1,
            hq_station_id: Some(AMARR_STATION_ID), members: [P1].into_iter().collect(),
            member_roles: HashMap::from([(P1, roles)]), ..Default::default()
        };
        corp.ensure_wallet_divisions();
        snap.corporations.insert(CORP_ID, corp);
        if let Some(ch) = snap.characters.get_mut(&P1) {
            ch.corporation_id = CORP_ID;
            ch.balance = IskCents(10_000_000_00);
            ch.station_id = Some(AMARR_STATION_ID);
            ch.solar_system_id = AMARR_SYSTEM_ID;
            ch.region_id = AMARR_REGION_ID;
            ch.loyalty_points.insert(1_000_035, 15_000);
            ch.loyalty_points.insert(1_000_036, 500);
            ch.loyalty_points.insert(1_000_125, 5_000);
        }
        snap.items.insert(720_001, make_item(720_001, 36, P1, AMARR_STATION_ID, InventoryFlag::Hangar, 100, false));
        snap.items.insert(720_002, make_item(720_002, 34, P1, AMARR_STATION_ID, InventoryFlag::Hangar, 50, false));
        snap.items.insert(720_003, make_item(720_003, 35, P1, AMARR_STATION_ID, InventoryFlag::Hangar, 50, false));
        insert_expired_order(snap, p2_bid, P2, 36, true, IskCents(500_00), 40, IskCents(20_000_00), base_u);
        insert_expired_order(snap, p2_ask, P2, 36, false, IskCents(50_00), 40, IskCents(0), base_u);
    });

    let mut s1 = login_select(&ctx, 9401, "demo", "demo-secret", P1).await;
    // Place P1 buy & sell orders with duration=1, then advance clock 86_500s to expire naturally
    let _buy_oid = rpc(&ctx, &mut s1, "marketProxy", "PlaceBuyOrder", &[
        PyRep::Integer(AMARR_STATION_ID), PyRep::Integer(35), PyRep::Real(100.0), PyRep::Integer(100),
        PyRep::Integer(-1), PyRep::Integer(1), PyRep::Integer(1), PyRep::Bool(false),
    ]).await.unwrap();
    let sell_oid = rpc(&ctx, &mut s1, "marketProxy", "PlaceMultiSellOrder", &[PyRep::List(vec![PyRep::Dict(vec![
        (PyRep::String("itemID".into()), PyRep::Integer(720_003)), (PyRep::String("typeID".into()), PyRep::Integer(35)),
        (PyRep::String("price".into()), PyRep::Real(200.0)), (PyRep::String("quantity".into()), PyRep::Integer(50)),
        (PyRep::String("stationID".into()), PyRep::Integer(AMARR_STATION_ID)), (PyRep::String("duration".into()), PyRep::Integer(1)),
    ])]), PyRep::Bool(false)]).await.unwrap().dict_get("orderID").and_then(|v| v.as_i64()).unwrap();
    let bal_escrowed = ctx.db.get_character(P1).unwrap().balance.0;
    let _ = ctx.drain_notifications_for_char(P1);
    ctx.advance_secs(86_500);
    // All 5 read RPCs exclude expired orders
    for m in ["GetRegionBest", "GetStationAsks", "GetCharOrders", "GetCorporationOrders"] {
        let rep = rpc(&ctx, &mut s1, "marketProxy", m, &[]).await.unwrap();
        assert!(!format!("{rep:?}").contains(&p2_ask.to_string()));
        assert!(!format!("{rep:?}").contains(&sell_oid.to_string()));
    }
    let ord_rep = rpc(&ctx, &mut s1, "marketProxy", "GetOrders", &[PyRep::Integer(36)]).await.unwrap();
    assert!(!format!("{ord_rep:?}").contains(&p2_bid.to_string()));

    // Refunded escrow + returned 50 units of type 35 to AMARR_STATION_ID hangar + OnMarketItemReceived
    let bal_after_exp = ctx.db.get_character(P1).unwrap().balance.0;
    assert_eq!(bal_after_exp - bal_escrowed, 10_000_00);
    ctx.db.with_snapshot(|s| {
        let pyerite_qty: i32 = s.items.values()
            .filter(|i| i.owner_id == P1 && i.location_id == AMARR_STATION_ID && i.flag == InventoryFlag::Hangar && i.type_id == 35)
            .map(|i| i.quantity)
            .sum();
        assert_eq!(pyerite_qty, 50);
    });
    let _ = rpc(&ctx, &mut s1, "marketProxy", "GetCharOrders", &[]).await.unwrap();
    assert_eq!(ctx.db.get_character(P1).unwrap().balance.0, bal_after_exp);
    let notifs = ctx.drain_notifications_for_char(P1);
    let item_recv = notifs.iter().find(|n| n.event == "OnMarketItemReceived").unwrap();
    assert_eq!(item_recv.args.last(), Some(&PyRep::Integer(AMARR_STATION_ID)));

    // All 3 match RPCs never match expired orders
    let bal_before = ctx.db.get_character(P1).unwrap().balance.0;
    let buy_m = rpc(&ctx, &mut s1, "marketProxy", "PlaceBuyOrder", &[PyRep::Integer(AMARR_STATION_ID), PyRep::Integer(36), PyRep::Real(100.0), PyRep::Integer(10), PyRep::Integer(32767), PyRep::Integer(1), PyRep::Integer(0), PyRep::Bool(false)]).await;
    assert!(buy_m.is_err() || buy_m.as_ref().ok().and_then(|r| r.dict_get("filledImmediately")).and_then(|v| v.as_bool()).unwrap_or(false) == false);
    let sell_err = rpc(&ctx, &mut s1, "marketProxy", "PlaceMultiSellOrder", &[PyRep::List(vec![PyRep::Dict(vec![
        (PyRep::String("itemID".into()), PyRep::Integer(720_001)), (PyRep::String("typeID".into()), PyRep::Integer(36)),
        (PyRep::String("price".into()), PyRep::Real(100.0)), (PyRep::String("quantity".into()), PyRep::Integer(10)),
        (PyRep::String("stationID".into()), PyRep::Integer(AMARR_STATION_ID)), (PyRep::String("duration".into()), PyRep::Integer(0)),
    ])]), PyRep::Bool(false)]).await.unwrap_err();
    assert_eq!(sell_err.code, "ImmediateOrderUnfilled");
    let _ = rpc(&ctx, &mut s1, "marketProxy", "BuyMultipleItems", &[PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(36), PyRep::Integer(10), PyRep::Real(100.0)])])]).await;
    assert_eq!(ctx.db.get_character(P1).unwrap().balance.0, bal_before);

    // LP durability + logs + atomic failed purchase
    rpc(&ctx, &mut s1, "LPSvc", "TransferLPFromMyWalletToOtherCorp", &[PyRep::Integer(CORP_ID), PyRep::Integer(1_000_035), PyRep::Integer(6_000)]).await.unwrap();
    rpc(&ctx, &mut s1, "LPSvc", "ExchangeConcordLP", &[PyRep::Integer(1_000_036), PyRep::Integer(1_000)]).await.unwrap();
    let corp_lp_before = rpc(&ctx, &mut s1, "LPSvc", "GetAllMyCorporationWalletLPBalances", &[]).await.unwrap();
    let logs_before = rpc(&ctx, &mut s1, "rewardMgr", "GetRewardLPLogs", &[]).await.unwrap();
    drop(ctx);

    let ctx2 = open_ctx_at(&db_path, base_u + 100);
    common::seed_test_lp_store_offers(&ctx2);
    let mut s1_re = login_select(&ctx2, 9402, "demo", "demo-secret", P1).await;
    let cn_station = ctx2.sde.stations.values().find(|s| s.owner_corporation_id == 1_000_035 && eve_server::services::station_structure::npc_station_offers_service(&ctx2.sde, s.station_id, 25)).unwrap().station_id;
    ctx2.db.with_mut_snapshot(|s| { if let Some(ch) = s.characters.get_mut(&P1) { ch.station_id = Some(cn_station); } });
    s1_re.station_id = Some(cn_station);
    assert_eq!(rpc(&ctx2, &mut s1_re, "LPSvc", "GetAllMyCorporationWalletLPBalances", &[]).await.unwrap(), corp_lp_before);
    assert_eq!(rpc(&ctx2, &mut s1_re, "rewardMgr", "GetRewardLPLogs", &[]).await.unwrap(), logs_before);

    let snap_before = ctx2.db.with_snapshot(|s| (s.characters[&P1].loyalty_points.clone(), s.corp_lp_balances.clone(), s.characters[&P1].balance, s.items[&720_002].quantity));
    assert_eq!(rpc(&ctx2, &mut s1_re, "LPStoreMgr", "TakeOfferForCharacter", &[PyRep::Integer(1_000_035), PyRep::Integer(1004), PyRep::Integer(1)]).await.unwrap_err().code, "MissingRequiredItems");
    assert_eq!(rpc(&ctx2, &mut s1_re, "LPStoreMgr", "TakeOfferForCharacter", &[PyRep::Integer(1_000_035), PyRep::Integer(1003), PyRep::Integer(1)]).await.unwrap_err().code, "InsufficientLoyaltyPoints");
    let snap_after = ctx2.db.with_snapshot(|s| (s.characters[&P1].loyalty_points.clone(), s.corp_lp_balances.clone(), s.characters[&P1].balance, s.items[&720_002].quantity));
    assert_eq!(snap_after, snap_before);
    let _ = std::fs::remove_file(&db_path);
}

#[tokio::test]
async fn test_b30_space_jetcan_mwd_module_cycles_and_mission_downtime() {
    // parity: feat:lifecycle.jetcan-empty, feat:lifecycle.mobile_warp_disruptor-lifecycle, feat:lifecycle.module-cycles-stop-on-leave-space, feat:lifecycle.mission-downtime-respawn
    eve_testkit::attest(&[
        "feat:lifecycle.jetcan-empty",
        "feat:lifecycle.mobile_warp_disruptor-lifecycle",
        "feat:lifecycle.module-cycles-stop-on-leave-space",
        "feat:lifecycle.mission-downtime-respawn",
    ]);

    let db_path = temp_db_path("space");
    let day_start = 1_800_000_000u64 - (1_800_000_000u64 % 86_400);
    let base_u = day_start + 11 * 3_600 - 5;
    let ctx = open_fresh_ctx_at(&db_path, base_u);
    let sys_id = JITA_SYSTEM_ID;
    let (ship_id, cargo_a, cargo_b, mod_id, gun_id, mwd_id) = (620_000_001i64, 620_000_010i64, 620_000_011i64, 620_000_020i64, 620_000_021i64, 620_000_030i64);

    let sys = ctx.systems.get_or_spawn(sys_id);
    let station_pos = ctx.sde.stations[&JITA_STATION_ID].position;
    let ship_ball = Ball { id: ship_id, type_id: 639, position: station_pos + DVec3::new(100.0, 0.0, 0.0), velocity: DVec3::ZERO, max_velocity: 300.0, inertia: 1.0, mass: 100_000_000.0, radius: 250.0, speed_fraction: 0.0, mode: MovementMode::Stop };
    sys.spawn_ball_with_meta(ship_ball.clone(), ctx.build_ship_combat_state(P1, ship_id, 639), ctx.build_ship_slim_meta(P1, 639)).await.unwrap();

    let sec_agent = ctx.sde.agents.values().find(|a| a.agent_type_id == 2 && a.division_id == 24 && a.level == 1 && a.station_id == Some(JITA_STATION_ID)).cloned()
        .or_else(|| ctx.sde.agents.values().find(|a| a.agent_type_id == 2 && a.division_id == 24 && a.level == 1 && a.station_id.is_some()).cloned()).unwrap();
    let ag_station = sec_agent.station_id.unwrap();
    let ag_sys = sec_agent.solar_system_id.unwrap();

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&P1) {
            ch.solar_system_id = sys_id; ch.station_id = None; ch.structure_id = None; ch.active_ship_id = ship_id;
            ch.standings.insert(sec_agent.agent_id, 9.0);
            ch.standings.insert(sec_agent.corporation_id, 9.0);
        }
        for stid in [11_584_u32, 3_435_u32, 3_436_u32, 3_300_u32, 3_301_u32, 3_311_u32] {
            snap.skills.entry(P1).or_default().insert(stid, TrainedSkillRecord { type_id: stid, level: 5, skill_points: 256_000.0 });
        }
        snap.items.insert(ship_id, make_item(ship_id, 639, P1, sys_id as i64, InventoryFlag::None, 1, true));
        snap.items.insert(cargo_a, make_item(cargo_a, 34, P1, ship_id, InventoryFlag::Cargo, 25, false));
        snap.items.insert(cargo_b, make_item(cargo_b, 34, P1, ship_id, InventoryFlag::Cargo, 25, false));
        snap.items.insert(mod_id, make_item(mod_id, 3528, P1, ship_id, InventoryFlag::LoSlot0, 1, true));
        for (i, flg) in [InventoryFlag::HiSlot0, InventoryFlag::HiSlot1, InventoryFlag::HiSlot2].into_iter().enumerate() {
            let gid = gun_id + (i as i64) * 2;
            snap.items.insert(gid, make_item(gid, 2929, P1, ship_id, flg, 1, true));
            snap.items.insert(gid + 1, make_item(gid + 1, 193, P1, ship_id, flg, 500, false));
            snap.module_online_states.insert(gid, true);
        }
        snap.items.insert(mwd_id, make_item(mwd_id, 12_198, P1, ship_id, InventoryFlag::Cargo, 1, true));
    });

    let mut sess = SessionState { user_id: Some(1), session_id: 9501, character_id: Some(P1), ship_id: Some(ship_id), solar_system_id: Some(sys_id), ..Default::default() };
    let _ = ctx.db.acquire_online_lock(1, 9501, Some(P1), false, base_u);

    // 1. `jetcan-empty`: last item removed deletes container item, ball, and broadcasts RemoveBall
    let jet_res = rpc(&ctx, &mut sess, "ship", "Jettison", &[PyRep::List(vec![PyRep::Integer(cargo_a), PyRep::Integer(cargo_b)])]).await.unwrap();
    let can_id = match &jet_res { PyRep::Tuple(t) => t.first().and_then(|v| match v { PyRep::List(l) => l.first().and_then(|x| x.as_i64()), _ => None }).unwrap(), _ => panic!() };
    let _ = sys.step_manual(1.0).await;
    rpc(&ctx, &mut sess, "invbroker", "Add", &[PyRep::Integer(cargo_a), PyRep::Integer(ship_id), PyRep::Integer(InventoryFlag::Cargo as i64)]).await.unwrap();
    assert!(sys.get_ball_snapshot(can_id).await.unwrap().is_some());
    ctx.drain_notifications_for_char(P1);
    rpc(&ctx, &mut sess, "invbroker", "Add", &[PyRep::Integer(cargo_b), PyRep::Integer(ship_id), PyRep::Integer(InventoryFlag::Cargo as i64)]).await.unwrap();
    ctx.db.with_snapshot(|s| assert!(!s.items.contains_key(&can_id)));
    assert!(sys.get_ball_snapshot(can_id).await.unwrap().is_none());
    assert!(ctx.drain_notifications_for_char(P1).iter().any(|n| n.event == "DoDestinyUpdate" && format!("{:?}", n.args).contains("RemoveBall")));

    // 2. `module-cycles-stop-on-leave-space`: Activate repeater, verify active cycle, Dock, verify cycle removed & deactivation emitted
    let act_rep = rpc(&ctx, &mut sess, "dogmaIM", "Activate", &[PyRep::Integer(mod_id), PyRep::String("repair".into())]).await.unwrap();
    assert_eq!(act_rep.dict_get("active"), Some(&PyRep::Bool(true)));
    ctx.db.with_snapshot(|s| assert!(s.module_active_states.contains(&mod_id)));
    let _ = sys.step_manual(1.0).await.unwrap();
    assert_eq!(sys.update_module_overload(ship_id, mod_id, false).await, Ok(true));
    ctx.advance_secs(12);
    ctx.drain_notifications_for_char(P1);
    rpc(&ctx, &mut sess, "stationSvc", "Dock", &[PyRep::Integer(JITA_STATION_ID)]).await.unwrap();
    assert_eq!(sess.station_id, Some(JITA_STATION_ID));
    ctx.db.with_snapshot(|s| assert!(!s.module_active_states.contains(&mod_id)));
    assert_eq!(sys.update_module_overload(ship_id, mod_id, false).await, Ok(false));
    assert!(ctx.drain_notifications_for_char(P1).iter().any(|n| n.event == "OnGodmaShipEffect"));

    // 3. `mission-downtime-respawn`: Accept real mission, warp in, kill NPC via dogmaIM, downtime resets & respawns on warp-in
    ctx.db.with_mut_snapshot(|s| {
        if let Some(ch) = s.characters.get_mut(&P1) { ch.station_id = Some(ag_station); ch.solar_system_id = ag_sys; }
        if let Some(sh) = s.items.get_mut(&ship_id) { sh.location_id = ag_station; }
    });
    sess.station_id = Some(ag_station);
    sess.solar_system_id = Some(ag_sys);
    rpc(&ctx, &mut sess, "agentMgr", "DoAction", &[PyRep::Integer(2), PyRep::Integer(sec_agent.agent_id)]).await.unwrap();
    let m_id = ctx.db.with_snapshot(|s| s.active_missions.keys().find_map(|&(c, mid)| (c == P1).then_some(mid)).unwrap());
    rpc(&ctx, &mut sess, "agentMgr", "DoAction", &[PyRep::Integer(3), PyRep::Integer(sec_agent.agent_id), PyRep::Integer(i64::from(m_id))]).await.unwrap();
    let m_sys = ctx.db.with_snapshot(|s| s.active_missions[&(P1, m_id)].solar_system_id);
    let ag_space = ctx.systems.get_or_spawn(m_sys);
    if m_sys == ag_sys {
        let _ = rpc(&ctx, &mut sess, "stationSvc", "Undock", &[]).await.unwrap();
    } else {
        ctx.db.with_mut_snapshot(|s| {
            if let Some(ch) = s.characters.get_mut(&P1) { ch.station_id = None; ch.solar_system_id = m_sys; }
            if let Some(sh) = s.items.get_mut(&ship_id) { sh.location_id = i64::from(m_sys); }
        });
        sess.station_id = None;
        sess.solar_system_id = Some(m_sys);
        ag_space.spawn_ball_with_meta(ship_ball.clone(), ctx.build_ship_combat_state(P1, ship_id, 587), ctx.build_ship_slim_meta(P1, 587)).await.unwrap();
    }
    let ag_beyonce = ctx.bind_object(&sess, "beyonce", i64::from(m_sys)).unwrap();
    ctx.dispatch_rpc(&mut sess, "beyonce", Some(ag_beyonce), "CmdWarpToStuff", &[PyRep::String("mission".into()), PyRep::Integer(i64::from(m_id))], &HashMap::new(), None).await.unwrap();
    for _ in 0..120 {
        ag_space.step_manual(1.0).await.unwrap();
        if matches!(ag_space.get_ball_snapshot(ship_id).await.unwrap(), Some((_, b)) if b.mode == MovementMode::Stop) {
            break;
        }
    }
    let (initial_npcs, first_npc, initial_npc_ids) = ctx.db.with_snapshot(|s| (
        s.active_missions[&(P1, m_id)].npcs_remaining,
        s.active_missions[&(P1, m_id)].spawned_npc_ball_ids[0],
        s.active_missions[&(P1, m_id)].spawned_npc_ball_ids.clone(),
    ));
    let mut initial_types = Vec::with_capacity(initial_npc_ids.len());
    for nid in &initial_npc_ids {
        initial_types.push(ag_space.get_ball_snapshot(*nid).await.unwrap().unwrap().1.type_id);
    }
    let npc_pos = ag_space.get_ball_snapshot(first_npc).await.unwrap().unwrap().1.position;
    let ship_pos = ag_space.get_ball_snapshot(ship_id).await.unwrap().unwrap().1.position;
    if ship_pos.distance(npc_pos) > 1_000.0 {
        let _ = ctx.dispatch_rpc(&mut sess, "beyonce", Some(ag_beyonce), "CmdStop", &[], &HashMap::new(), None).await;
        assert!(ag_space.set_ball_position(ship_id, npc_pos + DVec3::new(500.0, 0.0, 0.0)).await);
    }
    rpc(&ctx, &mut sess, "dogmaIM", "AddTarget", &[PyRep::Integer(first_npc)]).await.unwrap();
    for _ in 0..25 {
        if ag_space.get_target_locks(ship_id).await.unwrap().iter().any(|l| l.target_id == first_npc && l.locked) { break; }
        ag_space.step_manual(1.0).await.unwrap();
    }
    for gid in [gun_id, gun_id + 2, gun_id + 4] {
        rpc(&ctx, &mut sess, "dogmaIM", "Activate", &[PyRep::Integer(gid), PyRep::Integer(first_npc)]).await.expect("Activate launcher on first_npc");
    }
    for _ in 0..480 {
        if ag_space.get_ball_snapshot(first_npc).await.unwrap().is_none() { break; }
        ag_space.step_manual(1.0).await.unwrap();
    }
    assert!(ag_space.get_ball_snapshot(first_npc).await.unwrap().is_none(), "first_npc destroyed before downtime");
    ctx.db.with_snapshot(|s| assert_eq!(s.active_missions[&(P1, m_id)].npcs_remaining, initial_npcs - 1));
    ctx.advance_secs(10);
    run_daily_downtime_and_jobs(&ctx);
    ctx.db.with_snapshot(|s| assert_eq!(s.active_missions[&(P1, m_id)].npcs_remaining, initial_npcs));
    let ag_beyonce = ctx.bind_object(&sess, "beyonce", i64::from(m_sys)).unwrap();
    ctx.dispatch_rpc(&mut sess, "beyonce", Some(ag_beyonce), "CmdWarpToStuff", &[PyRep::String("mission".into()), PyRep::Integer(i64::from(m_id))], &HashMap::new(), None).await.unwrap();
    let respawned_ids = ctx.db.with_snapshot(|s| s.active_missions[&(P1, m_id)].spawned_npc_ball_ids.clone());
    assert_eq!(respawned_ids.len() as u32, initial_npcs);
    assert!(respawned_ids.iter().all(|&id| id > 200_000_000 && !initial_npc_ids.contains(&id)));
    let mut respawned_types = Vec::with_capacity(respawned_ids.len());
    for nid in &respawned_ids {
        respawned_types.push(ag_space.get_ball_snapshot(*nid).await.unwrap().unwrap().1.type_id);
    }
    assert_eq!(respawned_types, initial_types);
    assert!(with_combat_runtime(&ctx, |rt| respawned_ids.iter().all(|id| rt.npc_entities.contains_key(id))));

    // 4. `mobile_warp_disruptor-lifecycle`: Anchor completion from anchor_start_unix, bubble re-created after restart, removed after 2 days
    let null_sys_id = 30_004_608u32;
    let null_sys = ctx.systems.get_or_spawn(null_sys_id);
    null_sys.spawn_ball_with_meta(Ball { id: ship_id, type_id: 587, position: DVec3::new(50_000.0, 0.0, 0.0), velocity: DVec3::ZERO, max_velocity: 300.0, inertia: 1.0, mass: 1_000_000.0, radius: 50.0, speed_fraction: 0.0, mode: MovementMode::Stop }, ctx.build_ship_combat_state(P1, ship_id, 587), ctx.build_ship_slim_meta(P1, 587)).await.unwrap();
    ctx.db.with_mut_snapshot(|s| { s.characters.get_mut(&P1).unwrap().solar_system_id = null_sys_id; });
    sess.solar_system_id = Some(null_sys_id);
    rpc(&ctx, &mut sess, "posMgr", "AnchorStructure", &[PyRep::Integer(mwd_id), PyRep::Tuple(vec![PyRep::Real(50_000.0), PyRep::Real(0.0), PyRep::Real(0.0)])]).await.unwrap();
    ctx.advance_secs(120);
    assert_eq!(null_sys.is_ball_scrambled(ship_id).await.unwrap(), true);
    let mwd_u = ctx.now_unix();
    drop(ctx);

    let ctx2 = open_ctx_at(&db_path, mwd_u + 60);
    let null_sys2 = ctx2.systems.get_or_spawn(null_sys_id);
    assert!(null_sys2.get_ball_snapshot(mwd_id).await.unwrap().is_some());
    null_sys2.spawn_ball_with_meta(Ball { id: ship_id, type_id: 587, position: DVec3::new(50_000.0, 0.0, 0.0), velocity: DVec3::ZERO, max_velocity: 300.0, inertia: 1.0, mass: 1_000_000.0, radius: 50.0, speed_fraction: 0.0, mode: MovementMode::Stop }, ctx2.build_ship_combat_state(P1, ship_id, 587), ctx2.build_ship_slim_meta(P1, 587)).await.unwrap();
    assert_eq!(null_sys2.is_ball_scrambled(ship_id).await.unwrap(), true);
    ctx2.advance_secs(172_900);
    run_daily_downtime_and_jobs(&ctx2);
    ctx2.db.with_snapshot(|s| assert!(!s.items.contains_key(&mwd_id)));
    let _ = std::fs::remove_file(&db_path);
}

}
