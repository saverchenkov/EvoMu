//! End-to-end remediation tests for involuntary structure control release synchronization
//! (`OnSessionChanged` `"shipid"`, `OnStructureTargetRemoved`, live `SessionState.ship_id`,
//! corp-join control release, and delayed 5-minute profile transition control release).

use eve_db::{CorpApplicationRecord, CorporationRecord, InventoryFlag, InventoryItemRecord, IskCents, UpwellStructureRecord};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use eve_server::services::station_structure::{structure_lifecycle as life, structure_upwell_ops as ops};
use eve_server::services::structure_profile_services as svc_acl;
use glam::DVec3;
use std::collections::HashMap;
use std::sync::Arc;

const OWNER: i64 = 90_000_001;
const RANCER: u32 = 30_002_718;

async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    ctx.dispatch_rpc(sess, service, None, method, args, &HashMap::new(), None).await
}

fn kv_i64(rep: &PyRep, key: &str) -> i64 {
    rep.dict_get(key).and_then(|v| v.as_i64()).unwrap_or(0)
}

fn new_ctx() -> Arc<ServerContext> {
    let ctx = Arc::new(ServerContext::new());
    ctx.set_clock(Arc::new(eve_db::VirtualServerClock::with_fixed_base(1_800_000_000)));
    ctx
}

fn make_citadel(structure_id: i64, owner_corp_id: i64, name: &str, position: [f64; 3]) -> UpwellStructureRecord {
    UpwellStructureRecord {
        structure_id, type_id: 35833, owner_corp_id, solar_system_id: RANCER, name: name.into(),
        state: life::STATE_SHIELD_VULNERABLE.into(), position, quantum_core_installed: true,
        fuel_blocks: 1_200, shield_hp: 1_000_000.0, armor_hp: 1_000_000.0, hull_hp: 1_000_000.0,
        reinforced_until_unix: 0, last_damage_tick: 0, last_damage_unix: 0, self_destruct_at_unix: None,
    }
}

fn make_item(item_id: i64, type_id: u32, owner_id: i64, location_id: i64, flag: InventoryFlag, quantity: i32, singleton: bool) -> InventoryItemRecord {
    InventoryItemRecord { item_id, type_id, owner_id, location_id, flag, quantity, singleton, version: 1, ..Default::default() }
}

fn has_shipid_session_change(notifs: &[eve_server::router::PendingMachoNotification], expected_ship: i64) -> bool {
    notifs.iter().any(|n| {
        n.event == "OnSessionChanged"
            && n.args
                .get(2)
                .and_then(|d| d.dict_get("shipid"))
                .and_then(|t| match t {
                    PyRep::Tuple(pair) if pair.len() == 2 => pair[1].as_i64(),
                    _ => None,
                })
                == Some(expected_ship)
    })
}

#[tokio::test]
async fn test_remediated_involuntary_structure_control_release_session_sync() {
    // parity: feat:structures.board-structure, feat:structures.defense-settings, rpc:structureControl.TakeControl, rpc:structureControl.GetStructurePilot
    eve_testkit::attest(&[
        "feat:structures.board-structure",
        "feat:structures.defense-settings",
        "rpc:structureControl.TakeControl",
        "rpc:structureControl.GetStructurePilot",
    ]);

    let ctx = new_ctx();
    let station_id = ctx.sde.stations.values().find(|s| s.solar_system_id == RANCER).map(|s| s.station_id);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&OWNER) {
            ch.solar_system_id = RANCER;
            ch.station_id = station_id;
            if let Some(sh) = snap.items.get_mut(&ch.active_ship_id) {
                sh.location_id = station_id.unwrap_or(i64::from(RANCER));
            }
        }
    });
    let mut owner_sess = SessionState::new(8250);
    rpc(&ctx, &mut owner_sess, "authentication", "Login", &[PyRep::String("demo".into()), PyRep::String("demo-secret".into())]).await.unwrap();
    rpc(&ctx, &mut owner_sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(OWNER)]).await.unwrap();
    ctx.advance_secs(12);
    rpc(&ctx, &mut owner_sess, "corpRegistry", "AddCorporation", &[PyRep::String("Session Sync Corp".into()), PyRep::String("SSYN".into())]).await.unwrap();
    ctx.advance_virtual_clock(7 * 86_400 + 12);
    let corp_id = owner_sess.corporation_id.expect("corp");

    let st_id = 880_825_001;
    let dir_pilot: i64 = 90_000_051;
    let acl_pilot: i64 = 90_000_052;
    let join_pilot: i64 = 90_000_053;
    let other_ceo: i64 = 90_000_054;
    let dir_ship = 880_825_011;
    let acl_ship = 880_825_012;
    let join_ship = 880_825_013;
    let other_ceo_ship = 880_825_014;
    let old_corp_id = 98_825_055;
    let target_corp_id = 98_825_077;

    let now_init = ctx.now_unix();
    let online_pilots = [
        (dir_pilot, corp_id, dir_ship),
        (acl_pilot, 98_825_088, acl_ship),
        (join_pilot, old_corp_id, join_ship),
        (other_ceo, target_corp_id, other_ceo_ship),
    ];
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(st_id, make_citadel(st_id, corp_id, "Sync Citadel", [0.0, 5.0e9, 0.0]));
        for (cid, name, ticker) in [(old_corp_id, "Old Gunner Corp", "OLDG"), (target_corp_id, "New Target Corp", "NEWT")] {
            let mut c = CorporationRecord::default();
            c.corporation_id = cid;
            c.name = name.into();
            c.ticker = ticker.into();
            c.ceo_id = if cid == target_corp_id { other_ceo } else { 90_000_059 };
            c.members.insert(c.ceo_id);
            snap.corporations.insert(cid, c);
        }
        snap.corporations.get_mut(&old_corp_id).unwrap().members.insert(join_pilot);
        snap.corporations.get_mut(&corp_id).unwrap().members.insert(dir_pilot);
        snap.corporations.get_mut(&corp_id).unwrap().member_roles.insert(dir_pilot, eve_db::corp_roles::ROLE_DIRECTOR);

        for (idx, (cid, ccorp, shid)) in online_pilots.into_iter().enumerate() {
            let uid = (idx + 2) as u64;
            if let Some(mut acc) = snap.accounts.get("demo").cloned() {
                acc.user_id = uid;
                acc.username = format!("pilot_{uid}");
                snap.accounts.insert(acc.username.clone(), acc);
            }
            let mut ch = snap.characters.get(&OWNER).cloned().unwrap();
            ch.character_id = cid;
            ch.user_id = uid;
            ch.corporation_id = ccorp;
            ch.solar_system_id = RANCER;
            ch.structure_id = Some(st_id);
            ch.station_id = None;
            ch.active_ship_id = shid;
            snap.characters.insert(cid, ch);
            snap.items.insert(shid, make_item(shid, 587, cid, st_id, InventoryFlag::Hangar, 1, true));
        }
    });
    for (idx, (cid, _, _)) in online_pilots.into_iter().enumerate() {
        ctx.db.acquire_online_lock((idx + 2) as u64, (8251 + idx) as u64, Some(cid), true, now_init).unwrap();
    }
    life::sync_all_structures(&ctx);

    let dock_session = |sid: u64, uid: u64, cid: i64, ccorp: i64, role: u64, shid: i64| {
        let mut s = SessionState::new(sid);
        s.user_id = Some(uid);
        s.character_id = Some(cid);
        s.corporation_id = Some(ccorp);
        s.corp_role = role;
        s.solar_system_id = Some(RANCER);
        s.structure_id = Some(st_id);
        s.ship_id = Some(shid);
        s
    };

    // 1. Role loss emits OnSessionChanged("shipid") + OnStructureTargetRemoved and syncs live SessionState.ship_id.
    let mut dir_sess = dock_session(8251, 2, dir_pilot, corp_id, eve_db::corp_roles::ROLE_DIRECTOR, dir_ship);
    rpc(&ctx, &mut dir_sess, "structureControl", "TakeControl", &[PyRep::Integer(st_id)]).await.unwrap();
    assert_eq!(dir_sess.ship_id, Some(st_id));
    let _ = ctx.drain_notifications_for_char(dir_pilot);
    rpc(&ctx, &mut owner_sess, "corpRegistry", "UpdateMember", &[PyRep::Integer(dir_pilot), PyRep::Integer(0)]).await.unwrap();
    let dir_notifs = ctx.drain_notifications_for_char(dir_pilot);
    assert!(has_shipid_session_change(&dir_notifs, dir_ship), "Role loss must emit OnSessionChanged shipid");
    assert!(dir_notifs.iter().any(|n| n.event == "OnStructureTargetRemoved"), "Role loss must emit OnStructureTargetRemoved");
    ctx.sync_session(&mut dir_sess);
    assert_eq!(dir_sess.ship_id, Some(dir_ship), "Live SessionState.ship_id must restore on role loss");

    // 2. Configure defense ACL profile with acl_pilot and old_corp_id, then test ACL loss and corp-join control release.
    let acl_grp = kv_i64(&rpc(&ctx, &mut owner_sess, "ownerGroupManager", "Create", &[PyRep::String("Sync ACL".into()), PyRep::String("SACL".into())]).await.unwrap(), "groupID");
    rpc(&ctx, &mut owner_sess, "ownerGroupManager", "AddMembers", &[PyRep::Integer(acl_grp), PyRep::List(vec![PyRep::Integer(acl_pilot), PyRep::Integer(old_corp_id)])]).await.unwrap();
    let acl_prof = rpc(&ctx, &mut owner_sess, "structureProfiles", "CreateProfile", &[PyRep::String("ACL Prof".into()), PyRep::String("ACL".into())]).await.unwrap().as_i64().unwrap();
    let corp_only_prof = rpc(&ctx, &mut owner_sess, "structureProfiles", "CreateProfile", &[PyRep::String("Corp Only Prof".into()), PyRep::String("CORP".into())]).await.unwrap().as_i64().unwrap();
    rpc(&ctx, &mut owner_sess, "structureProfiles", "SaveProfileSettings", &[
        PyRep::Integer(acl_prof),
        PyRep::List(vec![
            PyRep::Tuple(vec![PyRep::Integer(19), PyRep::Integer(0), PyRep::Integer(1)]),
            PyRep::Tuple(vec![PyRep::Integer(17), PyRep::Integer(acl_grp), PyRep::Integer(1)]),
        ]),
    ]).await.unwrap();
    rpc(&ctx, &mut owner_sess, "structureProfiles", "SaveProfileSettings", &[
        PyRep::Integer(corp_only_prof),
        PyRep::List(vec![
            PyRep::Tuple(vec![PyRep::Integer(19), PyRep::Integer(0), PyRep::Integer(0)]),
        ]),
    ]).await.unwrap();
    rpc(&ctx, &mut owner_sess, "structureProfiles", "ChangeProfiles", &[PyRep::List(vec![PyRep::Integer(st_id)]), PyRep::Integer(acl_prof)]).await.unwrap();

    let mut acl_sess = dock_session(8252, 3, acl_pilot, 98_825_088, 0, acl_ship);
    rpc(&ctx, &mut acl_sess, "structureControl", "TakeControl", &[PyRep::Integer(st_id)]).await.unwrap();
    let _ = ctx.drain_notifications_for_char(acl_pilot);
    rpc(&ctx, &mut owner_sess, "ownerGroupManager", "DeleteMembers", &[PyRep::Integer(acl_grp), PyRep::List(vec![PyRep::Integer(acl_pilot)])]).await.unwrap();
    let acl_notifs = ctx.drain_notifications_for_char(acl_pilot);
    assert!(has_shipid_session_change(&acl_notifs, acl_ship), "ACL loss must emit OnSessionChanged shipid");
    ctx.sync_session(&mut acl_sess);
    assert_eq!(acl_sess.ship_id, Some(acl_ship), "Live SessionState.ship_id must restore on ACL loss");

    // 3. Corp join via UpdateApplicationOffer releases control when old corp loses ACL authorization.
    let mut join_sess = dock_session(8253, 4, join_pilot, old_corp_id, 0, join_ship);
    rpc(&ctx, &mut join_sess, "structureControl", "TakeControl", &[PyRep::Integer(st_id)]).await.unwrap();
    assert_eq!(join_sess.ship_id, Some(st_id));
    let app_id = 770_825_001;
    let now_u = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.get_mut(&target_corp_id).unwrap().applications.insert(
            app_id,
            CorpApplicationRecord {
                application_id: app_id,
                character_id: join_pilot,
                corporation_id: target_corp_id,
                message: "Joining new corp".into(),
                status: "Pending".into(),
                created_unix: now_u,
            },
        );
    });
    let _ = ctx.drain_notifications_for_char(join_pilot);
    let mut ceo_sess = dock_session(8254, 5, other_ceo, target_corp_id, eve_db::corp_roles::ROLE_DIRECTOR, other_ceo_ship);
    rpc(&ctx, &mut ceo_sess, "corpRegistry", "UpdateApplicationOffer", &[PyRep::Integer(app_id), PyRep::String("Accepted".into())]).await.unwrap();
    let join_notifs = ctx.drain_notifications_for_char(join_pilot);
    assert!(has_shipid_session_change(&join_notifs, join_ship), "Corp join must emit OnSessionChanged shipid");
    assert!(join_notifs.iter().any(|n| n.event == "OnStructureTargetRemoved"));
    ctx.sync_session(&mut join_sess);
    assert_eq!(join_sess.ship_id, Some(join_ship), "Live SessionState.ship_id must restore on corp join");

    // 4. Delayed 300s profile transition releases unauthorized gunner control and emits OnSessionChanged("shipid").
    rpc(&ctx, &mut owner_sess, "ownerGroupManager", "AddMembers", &[PyRep::Integer(acl_grp), PyRep::List(vec![PyRep::Integer(acl_pilot)])]).await.unwrap();
    rpc(&ctx, &mut acl_sess, "structureControl", "TakeControl", &[PyRep::Integer(st_id)]).await.unwrap();
    assert_eq!(acl_sess.ship_id, Some(st_id));
    rpc(&ctx, &mut owner_sess, "structureProfiles", "ChangeProfiles", &[PyRep::List(vec![PyRep::Integer(st_id)]), PyRep::Integer(corp_only_prof)]).await.unwrap();
    let _ = ctx.drain_notifications_for_char(acl_pilot);
    ctx.advance_virtual_clock(300);
    assert_eq!(rpc(&ctx, &mut owner_sess, "structureControl", "GetStructurePilot", &[PyRep::Integer(st_id)]).await.unwrap(), PyRep::None);
    let prof_notifs = ctx.drain_notifications_for_char(acl_pilot);
    assert!(has_shipid_session_change(&prof_notifs, acl_ship), "Delayed profile switch must emit OnSessionChanged shipid");
    ctx.sync_session(&mut acl_sess);
    assert_eq!(acl_sess.ship_id, Some(acl_ship), "Live SessionState.ship_id must restore after delayed profile switch");
}

#[tokio::test]
async fn test_remediated_bf01_bf02_upwell_market_acl_offline_filter_and_corp_div1_ledger() {
    // parity: feat:structures.profiles, feat:market.structure-markets, rpc:marketProxy.PlaceBuyOrder, rpc:marketProxy.PlaceMultiSellOrder, rpc:marketProxy.ModifyCharOrder, rpc:marketProxy.GetStationAsks, rpc:reprocessingSvc.Reprocess, rpc:jumpCloneSvc.InstallCloneInStructure
    eve_testkit::attest(&[
        "feat:structures.profiles",
        "feat:market.structure-markets",
        "rpc:marketProxy.PlaceBuyOrder",
        "rpc:marketProxy.PlaceMultiSellOrder",
        "rpc:marketProxy.ModifyCharOrder",
        "rpc:marketProxy.GetStationAsks",
        "rpc:reprocessingSvc.Reprocess",
        "rpc:jumpCloneSvc.InstallCloneInStructure",
    ]);

    let ctx = new_ctx();
    let mkt_sys: u32 = RANCER;
    let station_id = ctx.sde.stations.values().find(|s| s.solar_system_id == mkt_sys).map(|s| s.station_id);
    let region_id = ctx.sde.solar_systems.get(&mkt_sys).map(|s| s.region_id).unwrap_or(10_000_002);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&OWNER) {
            ch.solar_system_id = mkt_sys;
            ch.region_id = region_id;
            ch.station_id = station_id;
            if let Some(sh) = snap.items.get_mut(&ch.active_ship_id) {
                sh.location_id = station_id.unwrap_or(i64::from(mkt_sys));
            }
        }
    });
    let mut owner_sess = SessionState::new(8260);
    rpc(&ctx, &mut owner_sess, "authentication", "Login", &[PyRep::String("demo".into()), PyRep::String("demo-secret".into())]).await.unwrap();
    rpc(&ctx, &mut owner_sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(OWNER)]).await.unwrap();
    ctx.advance_secs(12);
    rpc(&ctx, &mut owner_sess, "corpRegistry", "AddCorporation", &[PyRep::String("Ledger Market Corp".into()), PyRep::String("LMKT".into())]).await.unwrap();
    ctx.advance_virtual_clock(7 * 86_400 + 12);
    let corp_id = owner_sess.corporation_id.expect("corp");

    let st_id: i64 = 1_000_880_826_001;
    let vip_cid: i64 = 90_000_061;
    let std_cid: i64 = 90_000_062;
    let dock_only_cid: i64 = 90_000_063;
    let ore_item_id: i64 = 880_826_301;
    let sell_item_id: i64 = 880_826_302;

    ctx.db.with_mut_snapshot(|snap| {
        let mut citadel = make_citadel(st_id, corp_id, "Market Ledger Citadel", [1.0e9, 0.0, 0.0]);
        citadel.solar_system_id = mkt_sys;
        snap.structures.insert(st_id, citadel);
        for (idx, (flag, srv_tid)) in [
            (InventoryFlag::ServiceSlot0, 35892_u32),
            (InventoryFlag::ServiceSlot1, 35888_u32),
            (InventoryFlag::ServiceSlot2, 35894_u32),
        ].into_iter().enumerate() {
            let mid = 880_826_100 + idx as i64;
            snap.items.insert(mid, make_item(mid, srv_tid, corp_id, st_id, flag, 1, true));
            snap.module_online_states.insert(mid, true);
        }
        for (idx, cid) in [vip_cid, std_cid, dock_only_cid].into_iter().enumerate() {
            let mut ch = snap.characters.get(&OWNER).cloned().unwrap();
            ch.character_id = cid;
            ch.corporation_id = 98_826_100 + idx as i64;
            ch.solar_system_id = mkt_sys;
            ch.region_id = region_id;
            ch.structure_id = Some(st_id);
            ch.station_id = None;
            ch.active_ship_id = 880_826_200 + idx as i64;
            ch.balance = IskCents(100_000_000_00);
            snap.characters.insert(cid, ch);
            snap.skills.entry(cid).or_default().insert(3446, eve_db::TrainedSkillRecord { type_id: 3446, level: 0, skill_points: 0.0 });
            snap.skills.entry(cid).or_default().insert(24242, eve_db::TrainedSkillRecord { type_id: 24242, level: 2, skill_points: 8_000.0 });
        }
        snap.items.insert(ore_item_id, make_item(ore_item_id, 1230, vip_cid, st_id, InventoryFlag::Hangar, 1_000, false));
        snap.items.insert(sell_item_id, make_item(sell_item_id, 34, vip_cid, st_id, InventoryFlag::Hangar, 2_000, false));
    });
    life::sync_all_structures(&ctx);

    let grp_dock = kv_i64(&rpc(&ctx, &mut owner_sess, "ownerGroupManager", "Create", &[PyRep::String("Dockers".into()), PyRep::String("DOCK".into())]).await.unwrap(), "groupID");
    let grp_vip = kv_i64(&rpc(&ctx, &mut owner_sess, "ownerGroupManager", "Create", &[PyRep::String("VIP Market".into()), PyRep::String("VIPM".into())]).await.unwrap(), "groupID");
    let grp_std = kv_i64(&rpc(&ctx, &mut owner_sess, "ownerGroupManager", "Create", &[PyRep::String("STD Market".into()), PyRep::String("STDM".into())]).await.unwrap(), "groupID");
    rpc(&ctx, &mut owner_sess, "ownerGroupManager", "AddMembers", &[PyRep::Integer(grp_dock), PyRep::List(vec![PyRep::Integer(vip_cid), PyRep::Integer(std_cid), PyRep::Integer(dock_only_cid)])]).await.unwrap();
    rpc(&ctx, &mut owner_sess, "ownerGroupManager", "AddMembers", &[PyRep::Integer(grp_vip), PyRep::List(vec![PyRep::Integer(vip_cid)])]).await.unwrap();
    rpc(&ctx, &mut owner_sess, "ownerGroupManager", "AddMembers", &[PyRep::Integer(grp_std), PyRep::List(vec![PyRep::Integer(std_cid)])]).await.unwrap();

    // Configure per-service ACLs directly on st_id via SaveProfileSettings:
    // VIP = 1% market / 2% repro / 100k clone; STD = 5% market; dock_only_cid has NO market ACL!
    rpc(&ctx, &mut owner_sess, "structureProfiles", "SaveProfileSettings", &[
        PyRep::Integer(st_id),
        PyRep::List(vec![
            PyRep::Tuple(vec![PyRep::Integer(svc_acl::SETTING_HOUSING_CAN_DOCK), PyRep::Integer(grp_dock), PyRep::Real(0.0)]),
            PyRep::Tuple(vec![PyRep::Integer(svc_acl::SETTING_MARKET_TAX), PyRep::Integer(grp_vip), PyRep::Real(0.01)]),
            PyRep::Tuple(vec![PyRep::Integer(svc_acl::SETTING_MARKET_TAX), PyRep::Integer(grp_std), PyRep::Real(0.05)]),
            PyRep::Tuple(vec![PyRep::Integer(svc_acl::SETTING_REPROCESSING_TAX), PyRep::Integer(grp_vip), PyRep::Real(0.02)]),
            PyRep::Tuple(vec![PyRep::Integer(svc_acl::SETTING_CLONINGBAY_TAX), PyRep::Integer(grp_vip), PyRep::Real(100_000.0)]),
        ]),
    ]).await.unwrap();

    let mk_sess = |sid: u64, cid: i64, corp: i64| {
        let mut s = SessionState::new(sid);
        s.user_id = Some(sid);
        s.character_id = Some(cid);
        s.corporation_id = Some(corp);
        s.solar_system_id = Some(mkt_sys);
        s.region_id = Some(region_id);
        s.structure_id = Some(st_id);
        s.ship_id = Some(880_826_200 + (sid - 8261) as i64);
        s
    };
    let mut sess_vip = mk_sess(8261, vip_cid, 98_826_100);
    let mut sess_std = mk_sess(8262, std_cid, 98_826_101);
    let mut sess_dock_only = mk_sess(8263, dock_only_cid, 98_826_102);

    let buy_args = [
        PyRep::Integer(st_id), PyRep::Integer(34), PyRep::Real(100.0), PyRep::Integer(10_000),
        PyRep::Integer(-1), PyRep::Integer(1), PyRep::Integer(90), PyRep::Bool(false),
    ];
    // Negative (BF-01): Pilot with docking access only is denied market orders
    assert_eq!(rpc(&ctx, &mut sess_dock_only, "marketProxy", "PlaceBuyOrder", &buy_args).await.unwrap_err().code, "StructureServiceAccessDenied");

    // Positive (BF-01 + BF-02): VIP (1% + 0.5% SCC = 15,000 ISK) and STD (5% + 0.5% SCC = 55,000 ISK) pay distinct per-ACL rates
    let corp_div1_before = ctx.db.with_snapshot(|s| s.corporations[&corp_id].division_balance(1).0);
    let vip_bal_0 = ctx.db.get_character(vip_cid).unwrap().balance.0;
    let std_bal_0 = ctx.db.get_character(std_cid).unwrap().balance.0;
    rpc(&ctx, &mut sess_vip, "marketProxy", "PlaceBuyOrder", &buy_args).await.unwrap();
    rpc(&ctx, &mut sess_std, "marketProxy", "PlaceBuyOrder", &buy_args).await.unwrap();
    assert_eq!(vip_bal_0 - ctx.db.get_character(vip_cid).unwrap().balance.0, 1_015_000_00);
    assert_eq!(std_bal_0 - ctx.db.get_character(std_cid).unwrap().balance.0, 1_055_000_00);
    // Owner corp div 1 receives 10,000 ISK (VIP 1%) + 50,000 ISK (STD 5%) = 60,000 ISK
    let corp_div1_after_buy = ctx.db.with_snapshot(|s| s.corporations[&corp_id].division_balance(1).0);
    assert_eq!(corp_div1_after_buy - corp_div1_before, 60_000_00);

    // Positive (BF-01): VIP places MultiSellOrder (1,000 x 200 ISK = 200k ISK -> 1% owner = 2k ISK + 0.5% SCC = 1k ISK)
    let sell_entry = PyRep::keyval([
        ("stationID", PyRep::Integer(st_id)),
        ("typeID", PyRep::Integer(34)),
        ("itemID", PyRep::Integer(sell_item_id)),
        ("price", PyRep::Real(200.0)),
        ("quantity", PyRep::Integer(1_000)),
        ("duration", PyRep::Integer(90)),
    ]);
    let vip_bal_1 = ctx.db.get_character(vip_cid).unwrap().balance.0;
    let (div1_pre_sell, cj_pre_sell, wj_pre_sell) = ctx.db.with_snapshot(|s| {
        (
            s.corporations[&corp_id].division_balance(1).0,
            s.corp_wallet_journals.get(&(corp_id, 1)).map_or(0, Vec::len),
            s.wallet_journal.len(),
        )
    });
    rpc(&ctx, &mut sess_vip, "marketProxy", "PlaceMultiSellOrder", &[PyRep::List(vec![sell_entry]), PyRep::Bool(false), PyRep::Integer(90)]).await.unwrap();
    assert_eq!(vip_bal_1 - ctx.db.get_character(vip_cid).unwrap().balance.0, 3_000_00);
    ctx.db.with_snapshot(|s| {
        assert_eq!(s.corporations[&corp_id].division_balance(1).0 - div1_pre_sell, 2_000_00, "PlaceMultiSellOrder must credit 2k ISK owner share to corp div 1");
        let cj = s.corp_wallet_journals.get(&(corp_id, 1)).expect("corp div 1 journal after PlaceMultiSellOrder");
        assert_eq!(cj.len(), cj_pre_sell + 1, "PlaceMultiSellOrder must append to corp_wallet_journals[(corp_id, 1)]");
        let entry = &cj[cj_pre_sell];
        assert_eq!(entry.ref_type, "MarketBrokerFee");
        assert!((entry.amount - 2_000.0).abs() < 1e-6);
        assert!(s.wallet_journal[wj_pre_sell..].iter().any(|e| e.character_id == corp_id && e.entry_type == "MarketBrokerFee" && e.amount.0 == 2_000_00));
    });
    let sell_oid = ctx.db.with_snapshot(|s| s.market_orders.values().find(|o| !o.is_bid && o.owner_id == vip_cid).unwrap().order_id);

    // Advance 301s past order modification cooldown and test ModifyCharOrder per-pilot broker fee
    ctx.advance_virtual_clock(301);
    let vip_bal_2 = ctx.db.get_character(vip_cid).unwrap().balance.0;
    rpc(&ctx, &mut sess_vip, "marketProxy", "ModifyCharOrder", &[PyRep::Integer(sell_oid), PyRep::Real(250.0)]).await.unwrap();
    // 1.5% * (0.5 * 250k + 50k) = 0.015 * 175k = 2,625 ISK
    assert_eq!(vip_bal_2 - ctx.db.get_character(vip_cid).unwrap().balance.0, 2_625_00);

    // Positive (BF-02): ReprocessingTax and JumpCloneInstallationFee credit corp division 1 AND corp_wallet_journals[(corp_id, 1)]
    let div1_pre_repro = ctx.db.with_snapshot(|s| s.corporations[&corp_id].division_balance(1).0);
    rpc(&ctx, &mut sess_vip, "reprocessingSvc", "Reprocess", &[PyRep::Integer(ore_item_id)]).await.unwrap();
    let div1_post_repro = ctx.db.with_snapshot(|s| s.corporations[&corp_id].division_balance(1).0);
    assert!(div1_post_repro > div1_pre_repro, "ReprocessingTax must credit corp division 1");

    rpc(&ctx, &mut sess_vip, "jumpCloneSvc", "InstallCloneInStructure", &[PyRep::Integer(st_id)]).await.unwrap();
    let div1_post_clone = ctx.db.with_snapshot(|s| s.corporations[&corp_id].division_balance(1).0);
    assert_eq!(div1_post_clone - div1_post_repro, 100_000_00, "JumpCloneInstallationFee must credit corp division 1");

    ctx.db.with_snapshot(|s| {
        let corp = &s.corporations[&corp_id];
        assert_eq!(corp.division_balance(1), corp.master_wallet);
        let cj = s.corp_wallet_journals.get(&(corp_id, 1)).expect("corp div 1 journal");
        assert!(cj.iter().any(|e| e.ref_type == "ReprocessingTax" && e.amount > 0.0));
        assert!(cj.iter().any(|e| e.ref_type == "JumpCloneInstallationFee" && (e.amount - 100_000.0).abs() < 1e-6));
        assert!(cj.iter().any(|e| e.ref_type == "MarketBrokerFee" && e.amount > 0.0));
        assert!(s.wallet_journal.iter().any(|e| e.character_id == corp_id && e.entry_type == "ReprocessingTax" && e.amount.0 > 0));
        assert!(s.wallet_journal.iter().any(|e| e.character_id == corp_id && e.entry_type == "JumpCloneInstallationFee" && e.amount.0 == 100_000_00));
    });

    // Negative (BF-01): Disabling market service in profile denies new orders, and taking Market Hub offline hides asks from GetStationAsks
    let rowset_len = |rep: &PyRep| -> usize {
        match rep {
            PyRep::ObjectEx { dict, .. } => dict.len(),
            PyRep::List(rows) => rows.len(),
            PyRep::Dict(d) => d.len(),
            _ => usize::MAX,
        }
    };
    let asks_online = rpc(&ctx, &mut sess_vip, "marketProxy", "GetStationAsks", &[PyRep::Integer(st_id)]).await.unwrap();
    assert_eq!(rowset_len(&asks_online), 1, "Online market hub ask must appear in GetStationAsks");

    ctx.db.with_mut_snapshot(|snap| {
        let mut ext = ops::load_ext_state(snap);
        ext.access_profiles.entry(st_id).or_default().general_settings.insert(svc_acl::SETTING_MARKET_TAX, false);
        ops::save_ext_state(snap, &ext);
    });
    assert_eq!(rpc(&ctx, &mut sess_vip, "marketProxy", "PlaceBuyOrder", &buy_args).await.unwrap_err().code, "StructureServiceAccessDenied");
    ctx.db.with_mut_snapshot(|snap| {
        let mut ext = ops::load_ext_state(snap);
        ext.access_profiles.entry(st_id).or_default().general_settings.remove(&svc_acl::SETTING_MARKET_TAX);
        ops::save_ext_state(snap, &ext);
        snap.module_online_states.insert(880_826_100, false);
    });
    let asks_offline = rpc(&ctx, &mut sess_vip, "marketProxy", "GetStationAsks", &[PyRep::Integer(st_id)]).await.unwrap();
    assert_eq!(rowset_len(&asks_offline), 0, "Offline market hub asks must be filtered from GetStationAsks");
    assert_eq!(rpc(&ctx, &mut sess_vip, "marketProxy", "PlaceBuyOrder", &buy_args).await.unwrap_err().code, "MarketHubOffline");

    // R-MKT-UPWELL-ELIGIBILITY-02: Tatara (35836) is market-capable; HullReinforced is treated as offline in GetStationAsks/GetSystemAsks/PlaceBuyOrder;
    // and external NPC station sellers without structure market ACL can fill ranged Upwell buy orders when online (and not when HullReinforced).
    let npc_sta = station_id.unwrap_or(60_003_760);
    let ext_item_id = 880_826_499;
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_online_states.insert(880_826_100, true);
        snap.structures.get_mut(&st_id).unwrap().type_id = 35836; // Tatara (Large Refinery)
        snap.skills.entry(vip_cid).or_default().insert(3447, eve_db::TrainedSkillRecord { type_id: 3447, level: 5, skill_points: 256_000.0 });
        snap.items.insert(ext_item_id, make_item(ext_item_id, 1230, dock_only_cid, npc_sta, InventoryFlag::Hangar, 500, false));
    });
    assert_eq!(rowset_len(&rpc(&ctx, &mut sess_vip, "marketProxy", "GetStationAsks", &[PyRep::Integer(st_id)]).await.unwrap()), 1, "Tatara (35836) with online Market Hub must expose asks");
    assert!(rowset_len(&rpc(&ctx, &mut sess_vip, "marketProxy", "GetSystemAsks", &[]).await.unwrap()) >= 1);
    let ranged_buy = [PyRep::Integer(st_id), PyRep::Integer(1230), PyRep::Real(120.0), PyRep::Integer(500), PyRep::Integer(32767), PyRep::Integer(1), PyRep::Integer(90), PyRep::Bool(false)];
    rpc(&ctx, &mut sess_vip, "marketProxy", "PlaceBuyOrder", &ranged_buy).await.expect("Tatara PlaceBuyOrder");
    ctx.db.with_mut_snapshot(|snap| snap.structures.get_mut(&st_id).unwrap().state = "HullReinforced".into());
    assert_eq!(rowset_len(&rpc(&ctx, &mut sess_vip, "marketProxy", "GetStationAsks", &[PyRep::Integer(st_id)]).await.unwrap()), 0, "HullReinforced structure market must be offline");
    assert_eq!(rpc(&ctx, &mut sess_vip, "marketProxy", "PlaceBuyOrder", &ranged_buy).await.unwrap_err().code, "MarketHubOffline");
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.get_mut(&st_id).unwrap().state = life::STATE_SHIELD_VULNERABLE.into();
        let ch = snap.characters.get_mut(&dock_only_cid).unwrap();
        ch.structure_id = None;
        ch.station_id = Some(npc_sta);
    });
    sess_dock_only.structure_id = None;
    sess_dock_only.station_id = Some(npc_sta);
    let ext_sell = PyRep::keyval([("stationID", PyRep::Integer(npc_sta)), ("typeID", PyRep::Integer(1230)), ("itemID", PyRep::Integer(ext_item_id)), ("price", PyRep::Real(110.0)), ("quantity", PyRep::Integer(200)), ("duration", PyRep::Integer(0))]);
    let dock_only_bal0 = ctx.db.get_character(dock_only_cid).unwrap().balance.0;
    rpc(&ctx, &mut sess_dock_only, "marketProxy", "PlaceMultiSellOrder", &[PyRep::List(vec![ext_sell]), PyRep::Bool(false), PyRep::Integer(0)]).await.expect("External NPC station seller fills ranged Tatara buy order without structure market ACL");
    assert!(ctx.db.get_character(dock_only_cid).unwrap().balance.0 > dock_only_bal0);
}

#[tokio::test]
async fn test_remediated_bf03_bf04_structure_control_immobility_and_direct_structure_profile_save() {
    // parity: feat:structures.board-structure, feat:structures.defense-settings, feat:structures.profiles, rpc:structureControl.TakeControl, rpc:beyonce.CmdWarpToStuff, rpc:structureDocking.Dock, rpc:abyssalMgr.AbyssalEntranceDeployment
    eve_testkit::attest(&[
        "feat:structures.board-structure",
        "feat:structures.defense-settings",
        "feat:structures.profiles",
        "rpc:structureControl.TakeControl",
        "rpc:beyonce.CmdWarpToStuff",
        "rpc:structureDocking.Dock",
        "rpc:abyssalMgr.AbyssalEntranceDeployment",
    ]);

    let ctx = new_ctx();
    let station_id = ctx.sde.stations.values().find(|s| s.solar_system_id == RANCER).map(|s| s.station_id);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&OWNER) {
            ch.solar_system_id = RANCER;
            ch.station_id = station_id;
            if let Some(sh) = snap.items.get_mut(&ch.active_ship_id) {
                sh.location_id = station_id.unwrap_or(i64::from(RANCER));
            }
        }
    });
    let mut owner_sess = SessionState::new(8270);
    rpc(&ctx, &mut owner_sess, "authentication", "Login", &[PyRep::String("demo".into()), PyRep::String("demo-secret".into())]).await.unwrap();
    rpc(&ctx, &mut owner_sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(OWNER)]).await.unwrap();
    ctx.advance_secs(12);
    rpc(&ctx, &mut owner_sess, "corpRegistry", "AddCorporation", &[PyRep::String("Defense Control Corp".into()), PyRep::String("DCTL".into())]).await.unwrap();
    ctx.advance_virtual_clock(7 * 86_400 + 12);
    let corp_id = owner_sess.corporation_id.expect("corp");

    let st_id = 880_827_001;
    let gunner_cid: i64 = 90_000_071;
    let blocked_cid: i64 = 90_000_072;
    let gunner_ship: i64 = 880_827_011;
    let blocked_ship: i64 = 880_827_012;
    let anchor_pos = [1.5e9, 2.5e9, 3.5e9];

    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(st_id, make_citadel(st_id, corp_id, "Immobile Citadel", anchor_pos));
        for (idx, (cid, shid)) in [(gunner_cid, gunner_ship), (blocked_cid, blocked_ship)].into_iter().enumerate() {
            let mut ch = snap.characters.get(&OWNER).cloned().unwrap();
            ch.character_id = cid;
            ch.corporation_id = 98_827_100 + idx as i64;
            ch.solar_system_id = RANCER;
            ch.structure_id = Some(st_id);
            ch.station_id = None;
            ch.active_ship_id = shid;
            snap.characters.insert(cid, ch);
            snap.items.insert(shid, make_item(shid, 587, cid, st_id, InventoryFlag::Hangar, 1, true));
        }
    });
    life::sync_all_structures(&ctx);

    let dock_grp = kv_i64(&rpc(&ctx, &mut owner_sess, "ownerGroupManager", "Create", &[PyRep::String("Direct Dock".into()), PyRep::String("DDCK".into())]).await.unwrap(), "groupID");
    let def_grp = kv_i64(&rpc(&ctx, &mut owner_sess, "ownerGroupManager", "Create", &[PyRep::String("Direct Def".into()), PyRep::String("DDEF".into())]).await.unwrap(), "groupID");
    rpc(&ctx, &mut owner_sess, "ownerGroupManager", "AddMembers", &[PyRep::Integer(dock_grp), PyRep::List(vec![PyRep::Integer(gunner_cid), PyRep::Integer(blocked_cid)])]).await.unwrap();
    rpc(&ctx, &mut owner_sess, "ownerGroupManager", "AddMembers", &[PyRep::Integer(def_grp), PyRep::List(vec![PyRep::Integer(gunner_cid)])]).await.unwrap();

    // Positive (BF-04): Direct SaveProfileSettings on structure_id persists docking ACLs, defense ACLs, and defense policy flags
    rpc(&ctx, &mut owner_sess, "structureProfiles", "SaveProfileSettings", &[
        PyRep::Integer(st_id),
        PyRep::List(vec![
            PyRep::Tuple(vec![PyRep::Integer(19), PyRep::Integer(0), PyRep::Integer(1)]),
            PyRep::Tuple(vec![PyRep::Integer(19), PyRep::Integer(dock_grp), PyRep::Integer(1)]),
            PyRep::Tuple(vec![PyRep::Integer(19), PyRep::Integer(gunner_cid), PyRep::Integer(1)]),
            PyRep::Tuple(vec![PyRep::Integer(19), PyRep::Integer(blocked_cid), PyRep::Integer(-1)]),
            PyRep::Tuple(vec![PyRep::Integer(17), PyRep::Integer(def_grp), PyRep::Integer(1)]),
            PyRep::Tuple(vec![PyRep::Integer(17), PyRep::Integer(blocked_cid), PyRep::Integer(-1)]),
            PyRep::Tuple(vec![PyRep::Integer(17), PyRep::Tuple(vec![PyRep::Bool(true), PyRep::Bool(true), PyRep::Bool(false), PyRep::Real(-2.5), PyRep::Bool(true)]), PyRep::Integer(0)]),
        ]),
    ]).await.unwrap();

    ctx.db.with_snapshot(|snap| {
        let ext = ops::load_ext_state(snap);
        let prof = ext.access_profiles.get(&st_id).expect("structure profile");
        assert!(prof.access_group_ids.contains(&dock_grp));
        assert!(prof.defense_group_ids.contains(&def_grp));
        assert!(prof.defense_blocked_characters.contains(&blocked_cid));
        let ds = eve_server::services::structure_acl_defense_rules::load_defense_settings(snap, st_id);
        assert!(ds.auto_defense_enabled);
        assert!(ds.auto_target_attackers);
        assert!((ds.standing_threshold - (-2.5)).abs() < 1e-6);
        assert!(ds.target_blocked_acl);
    });

    let mut gunner_sess = SessionState::new(8271);
    gunner_sess.user_id = Some(8271);
    gunner_sess.character_id = Some(gunner_cid);
    gunner_sess.corporation_id = Some(98_827_100);
    gunner_sess.solar_system_id = Some(RANCER);
    gunner_sess.structure_id = Some(st_id);
    gunner_sess.ship_id = Some(gunner_ship);

    let mut blocked_sess = SessionState::new(8272);
    blocked_sess.user_id = Some(8272);
    blocked_sess.character_id = Some(blocked_cid);
    blocked_sess.corporation_id = Some(98_827_101);
    blocked_sess.solar_system_id = Some(RANCER);
    blocked_sess.structure_id = Some(st_id);
    blocked_sess.ship_id = Some(blocked_ship);

    // Negative (BF-04): Blocked pilot cannot TakeControl; Positive: defenseGroupIDs pilot CAN TakeControl
    assert_eq!(rpc(&ctx, &mut gunner_sess, "structureSettings", "CharacterHasSetting", &[PyRep::Integer(st_id), PyRep::Integer(17)]).await.unwrap(), PyRep::Bool(true));
    assert_eq!(rpc(&ctx, &mut blocked_sess, "structureSettings", "CharacterHasSetting", &[PyRep::Integer(st_id), PyRep::Integer(17)]).await.unwrap(), PyRep::Bool(false));
    assert!(rpc(&ctx, &mut blocked_sess, "structureControl", "TakeControl", &[PyRep::Integer(st_id)]).await.is_err());
    rpc(&ctx, &mut gunner_sess, "structureControl", "TakeControl", &[PyRep::Integer(st_id)]).await.unwrap();
    assert_eq!(gunner_sess.ship_id, Some(st_id));

    // R-STRUCT-PENDING-PROFILE-SYNC-01: Delayed ChangeProfiles revokes unauthorized controller & syncs defense_settings when due
    let p_open = rpc(&ctx, &mut owner_sess, "structureProfiles", "CreateProfile", &[PyRep::String("P1".into()), PyRep::String("".into())]).await.unwrap().as_i64().unwrap();
    let p_lock = rpc(&ctx, &mut owner_sess, "structureProfiles", "CreateProfile", &[PyRep::String("P2".into()), PyRep::String("".into())]).await.unwrap().as_i64().unwrap();
    rpc(&ctx, &mut owner_sess, "structureProfiles", "SaveProfileSettings", &[PyRep::Integer(p_open), PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(19), PyRep::Integer(dock_grp), PyRep::Integer(1)]), PyRep::Tuple(vec![PyRep::Integer(17), PyRep::Integer(def_grp), PyRep::Integer(1)]), PyRep::Tuple(vec![PyRep::Integer(17), PyRep::Bool(true), PyRep::Integer(0)])])]).await.unwrap();
    rpc(&ctx, &mut owner_sess, "structureProfiles", "SaveProfileSettings", &[PyRep::Integer(p_lock), PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(19), PyRep::Integer(dock_grp), PyRep::Integer(1)]), PyRep::Tuple(vec![PyRep::Integer(17), PyRep::Bool(false), PyRep::Integer(0)])])]).await.unwrap();
    rpc(&ctx, &mut owner_sess, "structureProfiles", "ChangeProfiles", &[PyRep::List(vec![PyRep::Integer(st_id)]), PyRep::Integer(p_open)]).await.unwrap();
    rpc(&ctx, &mut owner_sess, "structureProfiles", "ChangeProfiles", &[PyRep::List(vec![PyRep::Integer(st_id)]), PyRep::Integer(p_lock)]).await.unwrap();
    ctx.advance_virtual_clock(305);
    assert_eq!(rpc(&ctx, &mut gunner_sess, "structureSettings", "CharacterHasSetting", &[PyRep::Integer(st_id), PyRep::Integer(17)]).await.unwrap(), PyRep::Bool(false));
    let _ = ctx.systems.get_or_spawn(RANCER).step_manual(1.0).await.unwrap();
    ctx.sync_session(&mut gunner_sess);
    assert_eq!(gunner_sess.ship_id, Some(gunner_ship), "Due pending profile transition on tick must release unauthorized controller");
    assert!(!ctx.db.with_snapshot(|s| eve_server::services::structure_acl_defense_rules::load_defense_settings(s, st_id).auto_defense_enabled));
    rpc(&ctx, &mut owner_sess, "structureProfiles", "SaveProfileSettings", &[PyRep::Integer(p_lock), PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(19), PyRep::Integer(dock_grp), PyRep::Integer(1)]), PyRep::Tuple(vec![PyRep::Integer(17), PyRep::Integer(def_grp), PyRep::Integer(1)])])]).await.unwrap();
    rpc(&ctx, &mut gunner_sess, "structureControl", "TakeControl", &[PyRep::Integer(st_id)]).await.unwrap();
    assert_eq!(gunner_sess.ship_id, Some(st_id));

    // Bind beyonce to RANCER and verify all navigation, warp, dock, and filament RPCs reject while controlling st_id (BF-03)
    let PyRep::Tuple(outer) = rpc(&ctx, &mut gunner_sess, "beyonce", "MachoBindObject", &[PyRep::Integer(i64::from(RANCER)), PyRep::None]).await.unwrap() else {
        panic!("expected Tuple from MachoBindObject");
    };
    let gunner_bind_id = outer[0].as_i64().map(|v| v as u64);

    assert_eq!(ctx.dispatch_rpc(&mut gunner_sess, "beyonce", gunner_bind_id, "CmdGotoPoint", &[PyRep::Real(0.0), PyRep::Real(0.0), PyRep::Real(0.0)], &HashMap::new(), None).await.unwrap_err().code, "StructureCannotNavigate");
    assert_eq!(ctx.dispatch_rpc(&mut gunner_sess, "beyonce", gunner_bind_id, "CmdGotoDirection", &[PyRep::Real(1.0), PyRep::Real(0.0), PyRep::Real(0.0)], &HashMap::new(), None).await.unwrap_err().code, "StructureCannotNavigate");
    assert_eq!(ctx.dispatch_rpc(&mut gunner_sess, "beyonce", gunner_bind_id, "CmdSetSpeedFraction", &[PyRep::Real(1.0)], &HashMap::new(), None).await.unwrap_err().code, "StructureCannotNavigate");
    assert_eq!(ctx.dispatch_rpc(&mut gunner_sess, "beyonce", gunner_bind_id, "CmdOrbit", &[PyRep::Integer(st_id), PyRep::Real(5_000.0)], &HashMap::new(), None).await.unwrap_err().code, "StructureCannotNavigate");
    assert_eq!(ctx.dispatch_rpc(&mut gunner_sess, "beyonce", gunner_bind_id, "CmdAlignTo", &[PyRep::Integer(station_id.unwrap_or(60_003_760) as i64)], &HashMap::new(), None).await.unwrap_err().code, "StructureCannotNavigate");
    assert_eq!(ctx.dispatch_rpc(&mut gunner_sess, "beyonce", gunner_bind_id, "CmdWarpToStuff", &[PyRep::String("item".into()), PyRep::Integer(station_id.unwrap_or(60_003_760) as i64)], &HashMap::new(), None).await.unwrap_err().code, "StructureCannotNavigate");
    assert_eq!(rpc(&ctx, &mut gunner_sess, "structureDocking", "Dock", &[PyRep::Integer(st_id)]).await.unwrap_err().code, "StructureCannotNavigate");
    assert_eq!(rpc(&ctx, &mut gunner_sess, "abyssalMgr", "AbyssalEntranceDeployment", &[PyRep::Integer(47_765)]).await.unwrap_err().code, "StructureCannotNavigate");

    // Verify structure ball in Ballpark never moved from anchor_pos
    let (_, st_ball) = ctx.systems.get_or_spawn(RANCER).get_ball_snapshot(st_id).await.unwrap().expect("structure ball");
    assert_eq!(st_ball.position, DVec3::from_array(anchor_pos));

    // Positive control: a normal piloted ship in space CAN issue CmdGotoPoint
    let free_ship: i64 = 880_827_099;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(free_ship, make_item(free_ship, 587, OWNER, i64::from(RANCER), InventoryFlag::Hangar, 1, true));
        if let Some(ch) = snap.characters.get_mut(&OWNER) {
            ch.active_ship_id = free_ship;
            ch.station_id = None;
            ch.structure_id = None;
        }
    });
    ctx.systems.get_or_spawn(RANCER).spawn_ball(Ball {
        id: free_ship,
        type_id: 587,
        position: DVec3::new(1.5e9 + 10_000.0, 2.5e9, 3.5e9),
        velocity: DVec3::ZERO,
        max_velocity: 350.0,
        inertia: 3.0,
        mass: 1_000_000.0,
        radius: 40.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    }).await.unwrap();
    let mut free_sess = SessionState::new(8273);
    free_sess.user_id = Some(8273);
    free_sess.character_id = Some(OWNER);
    free_sess.corporation_id = Some(corp_id);
    free_sess.solar_system_id = Some(RANCER);
    free_sess.ship_id = Some(free_ship);
    let PyRep::Tuple(free_outer) = rpc(&ctx, &mut free_sess, "beyonce", "MachoBindObject", &[PyRep::Integer(i64::from(RANCER)), PyRep::None]).await.unwrap() else {
        panic!("expected Tuple from MachoBindObject");
    };
    let free_bind_id = free_outer[0].as_i64().map(|v| v as u64);
    ctx.dispatch_rpc(&mut free_sess, "beyonce", free_bind_id, "CmdGotoPoint", &[PyRep::Real(0.0), PyRep::Real(0.0), PyRep::Real(0.0)], &HashMap::new(), None).await.expect("normal ship CmdGotoPoint succeeds");
}

