//! Remediation integration tests for 10 auditor-flagged rows across fleetObjectHandler and fleetMgr.

use std::collections::HashMap;
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use crate::common;

const PILOT1: i64 = 90_000_001;
const PILOT2: i64 = 90_000_002;

async fn setup_pilot(
    ctx: &ServerContext,
    sess_id: u64,
    user_name: &str,
    secret: &str,
    char_id: i64,
) -> SessionState {
    let mut sess = SessionState::new(sess_id);
    common::login_pilot(ctx, &mut sess, user_name, secret).await;
    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
        &HashMap::new(),
        None,
    )
    .await
    .expect("SelectCharacterID");
    sess
}

fn get_i64(rep: &PyRep, key: &str) -> Option<i64> {
    rep.dict_get(key).and_then(|v| v.as_i64())
}

fn get_bool(rep: &PyRep, key: &str) -> Option<bool> {
    rep.dict_get(key).and_then(|v| v.as_bool())
}

#[tokio::test]
async fn test_fleet_object_and_manager_remediation_lifecycle() {
    // parity: rpc:fleetObjectHandler.CreateFleet, rpc:fleetObjectHandler.FinishMove, rpc:fleetObjectHandler.GetInitState, rpc:fleetObjectHandler.GetJoinRequests, rpc:fleetObjectHandler.GetWings, rpc:fleetObjectHandler.Init, rpc:fleetObjectHandler.MakeLeader, rpc:fleetMgr.AddToWatchlist, rpc:fleetMgr.RegisterForDamageUpdates, rpc:fleetMgr.RemoveFromWatchlist
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let uid3 = ctx.db.provision_player_account("pilot3", "p3-sec");
    let pilot3 = ctx.db.create_character(uid3, "Pilot Three", 1, 1, 11, 17, &ctx.sde).expect("create pilot3").character_id;
    let uid4 = ctx.db.provision_player_account("pilot4", "p4-sec");
    let pilot4 = ctx.db.create_character(uid4, "Pilot Four", 1, 1, 11, 17, &ctx.sde).expect("create pilot4").character_id;

    let mut sess1 = setup_pilot(&ctx, 2011, "demo", "demo-secret", PILOT1).await;
    let mut sess2 = setup_pilot(&ctx, 2012, "other", "other-secret", PILOT2).await;
    let mut sess3 = setup_pilot(&ctx, 2013, "pilot3", "p3-sec", pilot3).await;
    let mut sess4 = setup_pilot(&ctx, 2014, "pilot4", "p4-sec", pilot4).await;
    let kw = HashMap::new();

    // 1. CreateFleet (0-arg client arity returns fleetID integer) & Init
    let _ = ctx.drain_notifications_for_char(PILOT1);
    let create_rep = ctx.dispatch_rpc(&mut sess1, "fleetObjectHandler", None, "CreateFleet", &[], &kw, None).await.expect("CreateFleet 0-arg");
    let fid1 = create_rep.as_i64().expect("0-arg fleetObjectHandler.CreateFleet must return fleetID integer");
    assert_eq!(sess1.fleet_id, Some(fid1));
    assert_eq!(
        ctx.drain_notifications_for_char(PILOT1).iter().filter(|n| n.event == "OnFleetJoin").count(),
        1,
        "CreateFleet must emit OnFleetJoin once to creator"
    );

    // Set Pilot 1's active ship in DB to a Rifter (587) and train Leadership (3348=4), Wing Command (11574=3), Fleet Command (24764=2)
    let ship1_id = ctx.db.with_mut_snapshot(|snap| {
        let sid = snap.characters.get(&PILOT1).map(|c| c.active_ship_id).unwrap_or(100_000_001);
        if let Some(sh) = snap.items.get_mut(&sid) {
            sh.type_id = 587;
        }
        let sk = snap.skills.entry(PILOT1).or_default();
        sk.insert(3348, eve_db::TrainedSkillRecord { type_id: 3348, level: 4, skill_points: 45_255.0 });
        sk.insert(11_574, eve_db::TrainedSkillRecord { type_id: 11_574, level: 3, skill_points: 16_000.0 });
        sk.insert(24_764, eve_db::TrainedSkillRecord { type_id: 24_764, level: 2, skill_points: 4_243.0 });
        sid
    });
    let fleet_cnt_before = ctx.db.with_snapshot(|s| s.fleets.len());
    let init_rep = ctx.dispatch_rpc(&mut sess1, "fleetObjectHandler", None, "Init", &[PyRep::Integer(606)], &kw, None).await.expect("Init without adInfoData");
    assert_eq!(init_rep, PyRep::None, "Init must return None");
    assert_eq!(ctx.db.with_snapshot(|s| s.fleets.len()), fleet_cnt_before, "Init must not create a second fleet");
    assert!(
        ctx.drain_notifications_for_char(PILOT1).iter().any(|n| n.event == "OnFleetJoin"),
        "Init on newly created fleet must emit OnFleetJoin"
    );
    assert_eq!(sess1.fleet_id, Some(fid1));

    let ad_kwargs = HashMap::from([(
        "adInfoData".to_string(),
        PyRep::Dict(vec![
            (PyRep::String("fleetName".into()), PyRep::String("Init Advert".into())),
            (PyRep::String("description".into()), PyRep::String("Via Init".into())),
        ]),
    )]);
    let init_ad_rep = ctx.dispatch_rpc(&mut sess1, "fleetObjectHandler", None, "Init", &[PyRep::Integer(606)], &ad_kwargs, None).await.expect("Init with adInfoData");
    assert_eq!(init_ad_rep, PyRep::None);
    let my_ad = ctx.dispatch_rpc(&mut sess1, "fleetProxy", None, "GetMyFleetFinderAdvert", &[], &kw, None).await.expect("GetMyFleetFinderAdvert");
    let PyRep::Rowset { rows: ad_rows, .. } = &my_ad else { panic!("expected Rowset from GetMyFleetFinderAdvert") };
    assert_eq!(ad_rows.len(), 1);
    assert_eq!(ad_rows[0][0], PyRep::Integer(fid1));
    assert_eq!(ad_rows[0][1], PyRep::Integer(PILOT1));
    assert_eq!(ad_rows[0][2], PyRep::String("Init Advert".into()));
    assert_eq!(ad_rows[0][4], PyRep::Integer(sess1.solar_system_id.unwrap() as i64));

    // Extract initial unique wingID and squadID from GetWings
    let initial_wings_rep = ctx.dispatch_rpc(&mut sess1, "fleetObjectHandler", None, "GetWings", &[], &kw, None).await.expect("GetWings initial");
    let PyRep::Dict(initial_wings) = &initial_wings_rep else { panic!("expected wings Dict") };
    assert_eq!(initial_wings.len(), 1);
    let wid1 = initial_wings[0].0.as_i64().expect("initial wingID");
    assert!(wid1 > 1, "0-arg CreateFleet must allocate a unique wingID > 1, got {wid1}");
    assert_eq!(initial_wings[0].1.dict_get("name").and_then(|v| v.as_str()), Some("Wing 1"));
    let PyRep::Dict(initial_squads) = initial_wings[0].1.dict_get("squads").expect("squads Dict") else { panic!("expected squads Dict") };
    assert_eq!(initial_squads.len(), 1);
    let sid1 = initial_squads[0].0.as_i64().expect("initial squadID");
    assert!(sid1 > 1 && sid1 != wid1, "0-arg CreateFleet must allocate a unique squadID > 1, got {sid1}");
    assert_eq!(initial_squads[0].1.dict_get("name").and_then(|v| v.as_str()), Some("Squad 1"));

    // Configure fleet options (isVoiceEnabled=true, isLootLogging=true) and MoveMember
    ctx.dispatch_rpc(
        &mut sess1,
        "fleetObjectHandler",
        None,
        "SetOptions",
        &[PyRep::keyval([
            ("isFreeMove", PyRep::Bool(false)),
            ("isRegistered", PyRep::Bool(false)),
            ("isVoiceEnabled", PyRep::Bool(true)),
            ("isLootLogging", PyRep::Bool(true)),
        ])],
        &kw,
        None,
    )
    .await
    .expect("SetOptions");
    ctx.dispatch_rpc(
        &mut sess1,
        "fleetObjectHandler",
        None,
        "MoveMember",
        &[PyRep::Integer(PILOT1), PyRep::Integer(wid1), PyRep::Integer(sid1), PyRep::Integer(1)],
        &kw,
        None,
    )
    .await
    .expect("MoveMember");

    // 2. GetInitState: returns options KeyVal, wings Dict, and members Dict with DB-backed skills/ship
    let init_st = ctx
        .dispatch_rpc(
            &mut sess1,
            "fleetObjectHandler",
            None,
            "GetInitState",
            &[],
            &kw,
            None,
        )
        .await
        .expect("GetInitState");

    let options_kv = init_st.dict_get("options").expect("options KeyVal");
    assert_eq!(get_bool(options_kv, "isFreeMove"), Some(false));
    assert_eq!(get_bool(options_kv, "isRegistered"), Some(false));
    assert_eq!(get_bool(options_kv, "isVoiceEnabled"), Some(true));
    assert_eq!(get_bool(&init_st, "isLootLogging"), Some(true));

    let PyRep::Dict(wings_dict) = init_st.dict_get("wings").expect("wings Dict") else {
        panic!("expected wings Dict");
    };
    assert!(!wings_dict.is_empty());

    let PyRep::Dict(members_dict) = init_st.dict_get("members").expect("members Dict") else {
        panic!("expected members Dict");
    };
    let boss_m = members_dict
        .iter()
        .find_map(|(k, v)| (k == &PyRep::Integer(PILOT1)).then_some(v))
        .expect("boss in members Dict");
    assert_eq!(get_i64(boss_m, "job"), Some(2)); // boss bit
    assert_eq!(get_i64(boss_m, "role"), Some(1)); // FleetCommander
    assert_eq!(get_i64(boss_m, "roleBooster"), Some(0)); // V24 uses command bursts
    assert_eq!(get_i64(boss_m, "shipTypeID"), Some(587)); // Active ship in DB, ignoring client Init(606)
    assert_eq!(
        boss_m.dict_get("skills"),
        Some(&PyRep::List(vec![PyRep::Integer(4), PyRep::Integer(3), PyRep::Integer(2)])),
        "GetInitState must return trained [Leadership, WingCommand, FleetCommand] skill levels"
    );

    // When character has no active ship item in DB, shipTypeID is None (never fabricated 606)
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&PILOT1) {
            c.active_ship_id = 0;
        }
    });
    let init_st_no_ship = ctx.dispatch_rpc(&mut sess1, "fleetObjectHandler", None, "GetInitState", &[], &kw, None).await.expect("GetInitState no ship");
    let PyRep::Dict(members_ns) = init_st_no_ship.dict_get("members").unwrap() else { panic!() };
    let boss_ns = members_ns.iter().find_map(|(k, v)| (k == &PyRep::Integer(PILOT1)).then_some(v)).unwrap();
    assert_eq!(boss_ns.dict_get("shipTypeID"), Some(&PyRep::None));
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&PILOT1) {
            c.active_ship_id = ship1_id;
        }
    });

    // 3. GetWings: returns dict of wingID -> KeyVal(wingID, name, squads Dict) and reflects wing/squad mutations
    let cw_rep = ctx.dispatch_rpc(&mut sess1, "fleetObjectHandler", None, "CreateWing", &[PyRep::String("Alpha Wing".into())], &kw, None).await.expect("CreateWing");
    let wid2 = get_i64(&cw_rep, "wingID").expect("wid2");
    let cs_rep = ctx.dispatch_rpc(&mut sess1, "fleetObjectHandler", None, "CreateSquad", &[PyRep::Integer(wid2), PyRep::String("Bravo Squad".into())], &kw, None).await.expect("CreateSquad");
    let sid2 = get_i64(&cs_rep, "squadID").expect("sid2");
    let wings_rep = ctx.dispatch_rpc(&mut sess1, "fleetObjectHandler", None, "GetWings", &[], &kw, None).await.expect("GetWings after CreateWing/CreateSquad");
    let PyRep::Dict(wings_after_add) = &wings_rep else { panic!("expected wings Dict") };
    assert_eq!(wings_after_add.len(), 2);
    let w2_kv = wings_after_add.iter().find_map(|(k, v)| (k == &PyRep::Integer(wid2)).then_some(v)).expect("wid2 in GetWings");
    assert_eq!(get_i64(w2_kv, "wingID"), Some(wid2));
    assert_eq!(w2_kv.dict_get("name").and_then(|v| v.as_str()), Some("Alpha Wing"));
    let PyRep::Dict(w2_squads) = w2_kv.dict_get("squads").expect("w2 squads Dict") else { panic!("expected squads Dict") };
    let s2_kv = w2_squads.iter().find_map(|(k, v)| (k == &PyRep::Integer(sid2)).then_some(v)).expect("sid2 in w2 squads");
    assert_eq!(get_i64(s2_kv, "squadID"), Some(sid2));
    assert_eq!(s2_kv.dict_get("name").and_then(|v| v.as_str()), Some("Bravo Squad"));
    ctx.dispatch_rpc(&mut sess1, "fleetObjectHandler", None, "DeleteSquad", &[PyRep::Integer(sid2)], &kw, None).await.expect("DeleteSquad");
    ctx.dispatch_rpc(&mut sess1, "fleetObjectHandler", None, "DeleteWing", &[PyRep::Integer(wid2)], &kw, None).await.expect("DeleteWing");
    let wings_after_del = ctx.dispatch_rpc(&mut sess1, "fleetObjectHandler", None, "GetWings", &[], &kw, None).await.expect("GetWings after DeleteWing");
    let PyRep::Dict(wings_del_dict) = &wings_after_del else { panic!("expected wings Dict") };
    assert_eq!(wings_del_dict.len(), 1);
    assert!(!wings_del_dict.iter().any(|(k, _)| k == &PyRep::Integer(wid2)), "Deleted wing must be removed from GetWings");

    // 4. Advert and Join Requests vs Invites + SQLite persistence of join_requests
    ctx.dispatch_rpc(
        &mut sess1, "fleetProxy", None, "AddFleetFinderAdvert",
        &[PyRep::String("Join Advert".into()), PyRep::String("Apply now".into()), PyRep::Integer(3), PyRep::Bool(false)],
        &kw, None,
    ).await.expect("AddFleetFinderAdvert");

    // Pilot 3 and Pilot 4 apply to join
    ctx.dispatch_rpc(&mut sess3, "fleetProxy", None, "ApplyToJoinFleet", &[PyRep::Integer(fid1)], &kw, None).await.expect("ApplyToJoinFleet Pilot 3");
    ctx.dispatch_rpc(&mut sess4, "fleetProxy", None, "ApplyToJoinFleet", &[PyRep::Integer(fid1)], &kw, None).await.expect("ApplyToJoinFleet Pilot 4");

    // Pilot 2 is merely invited
    ctx.dispatch_rpc(
        &mut sess1, "fleetObjectHandler", None, "Invite",
        &[PyRep::Integer(PILOT2), PyRep::Integer(wid1), PyRep::Integer(sid1), PyRep::Integer(4)],
        &kw, None,
    ).await.expect("Invite Pilot 2");

    // GetJoinRequests returns ONLY applicants (Pilot 3 & Pilot 4), NOT invitees (Pilot 2)
    let reqs_rep = ctx.dispatch_rpc(&mut sess1, "fleetObjectHandler", None, "GetJoinRequests", &[], &kw, None).await.expect("GetJoinRequests");
    let PyRep::Dict(reqs_dict) = reqs_rep else { panic!("expected dict") };
    assert!(reqs_dict.iter().any(|(k, _)| k == &PyRep::Integer(pilot3)));
    assert!(reqs_dict.iter().any(|(k, _)| k == &PyRep::Integer(pilot4)));
    assert!(!reqs_dict.iter().any(|(k, _)| k == &PyRep::Integer(PILOT2)));

    // RejectJoinRequest emits OnFleetJoinRejected(rejecterID), NOT OnFleetJoinReject, and persists removal in SQLite
    let _ = ctx.drain_notifications_for_char(pilot3);
    ctx.dispatch_rpc(&mut sess1, "fleetObjectHandler", None, "RejectJoinRequest", &[PyRep::Integer(pilot3)], &kw, None).await.expect("RejectJoinRequest");
    let p3_reject_notifs = ctx.drain_notifications_for_char(pilot3);
    assert!(!p3_reject_notifs.iter().any(|n| n.event == "OnFleetJoinReject"));
    let rej_notif = p3_reject_notifs.iter().find(|n| n.event == "OnFleetJoinRejected").expect("OnFleetJoinRejected");
    assert_eq!(rej_notif.args, vec![PyRep::Integer(PILOT1)]);

    // Pilot 2 accepts invite and joins fleet -> emits OnFleetJoin to fleet members
    let _ = ctx.drain_notifications_for_char(PILOT1);
    ctx.dispatch_rpc(&mut sess2, "fleetObjectHandler", None, "AcceptInvite", &[PyRep::Integer(fid1)], &kw, None).await.expect("AcceptInvite");
    assert!(ctx.drain_notifications_for_char(PILOT1).iter().any(|n| n.event == "OnFleetJoin"));

    // Persist to SQLite, reload blobs from SQLite, re-hydrate social state, and verify GetJoinRequests hydrates only Pilot 4
    let pid = std::process::id();
    let json_path = std::env::temp_dir().join(format!("evomu_fleet_reqs_{pid}.json"));
    let sqlite_path = std::env::temp_dir().join(format!("evomu_fleet_reqs_{pid}.sqlite"));
    let _ = std::fs::remove_file(&json_path);
    let _ = std::fs::remove_file(&sqlite_path);
    ctx.db.save_snapshot_to_file(&json_path).expect("save_snapshot_to_file");
    {
        let writer = eve_db::WorldRepository::open_sqlite(&sqlite_path).expect("open_sqlite writer");
        writer.load_snapshot_from_file(&json_path).expect("load_snapshot_from_file");
    }
    let repo_reloaded = eve_db::WorldRepository::open_sqlite(&sqlite_path).expect("reopen_sqlite");
    let snap_reloaded = repo_reloaded.with_snapshot(|s| s.clone());
    let _ = std::fs::remove_file(&json_path);
    let _ = std::fs::remove_file(&sqlite_path);
    ctx.db.with_mut_snapshot(|s| s.service_state_blobs = snap_reloaded.service_state_blobs);
    eve_server::services::fleet_social::hydrate_social_state_from_db(&ctx);
    let reqs_after_reload = ctx.dispatch_rpc(&mut sess1, "fleetObjectHandler", None, "GetJoinRequests", &[], &kw, None).await.expect("GetJoinRequests after SQLite reload");
    let PyRep::Dict(reqs_reloaded_dict) = reqs_after_reload else { panic!("expected dict") };
    assert!(!reqs_reloaded_dict.iter().any(|(k, _)| k == &PyRep::Integer(pilot3)), "Rejected applicant must remain absent after SQLite reload");
    assert!(reqs_reloaded_dict.iter().any(|(k, _)| k == &PyRep::Integer(pilot4)), "Remaining applicant must survive SQLite reload");

    // Non-commander Pilot 2 cannot call GetJoinRequests
    let unauth_reqs = ctx.dispatch_rpc(&mut sess2, "fleetObjectHandler", None, "GetJoinRequests", &[], &kw, None).await.unwrap_err();
    assert_eq!(unauth_reqs.code, "FleetPermissionDenied");

    // 5. FinishMove with 0 args leaves caller role untouched
    let fm_rep = ctx.dispatch_rpc(&mut sess1, "fleetObjectHandler", None, "FinishMove", &[], &kw, None).await.expect("FinishMove");
    assert_eq!(fm_rep, PyRep::None);

    let init_after_fm = ctx.dispatch_rpc(&mut sess1, "fleetObjectHandler", None, "GetInitState", &[], &kw, None).await.expect("GetInitState after FinishMove");
    let PyRep::Dict(m_dict) = init_after_fm.dict_get("members").unwrap() else { panic!() };
    let boss_after = m_dict.iter().find_map(|(k, v)| (k == &PyRep::Integer(PILOT1)).then_some(v)).unwrap();
    assert_eq!(get_i64(boss_after, "role"), Some(1)); // Still FleetCommander

    // 6. MakeLeader: transfers boss, moves job bit 2, and emits OnFleetMemberChanged + OnChangeFleetBoss
    let unauth_lead = ctx.dispatch_rpc(&mut sess2, "fleetObjectHandler", None, "MakeLeader", &[PyRep::Integer(PILOT1)], &kw, None).await.unwrap_err();
    assert_eq!(unauth_lead.code, "FleetPermissionDenied");

    let _ = ctx.drain_notifications_for_char(PILOT2);
    let make_lead = ctx.dispatch_rpc(&mut sess1, "fleetObjectHandler", None, "MakeLeader", &[PyRep::Integer(PILOT2)], &kw, None).await.expect("MakeLeader");
    assert_eq!(get_i64(&make_lead, "newBossID"), Some(PILOT2));
    let lead_notifs = ctx.drain_notifications_for_char(PILOT2);
    assert!(lead_notifs.iter().any(|n| n.event == "OnFleetMemberChanged"));
    assert!(lead_notifs.iter().any(|n| n.event == "OnChangeFleetBoss" && n.args == vec![PyRep::Integer(PILOT1), PyRep::Integer(PILOT2)]));

    let init_after_lead = ctx.dispatch_rpc(&mut sess2, "fleetObjectHandler", None, "GetInitState", &[], &kw, None).await.expect("GetInitState after MakeLeader");
    let PyRep::Dict(m_dict2) = init_after_lead.dict_get("members").unwrap() else { panic!() };
    let new_boss_m = m_dict2.iter().find_map(|(k, v)| (k == &PyRep::Integer(PILOT2)).then_some(v)).unwrap();
    assert_eq!(get_i64(new_boss_m, "job"), Some(2));
    assert_eq!(get_i64(new_boss_m, "role"), Some(4), "MakeLeader preserves new boss hierarchy role");
    let old_boss_m = m_dict2.iter().find_map(|(k, v)| (k == &PyRep::Integer(PILOT1)).then_some(v)).unwrap();
    assert_eq!(get_i64(old_boss_m, "job"), Some(0));
    assert_eq!(get_i64(old_boss_m, "role"), Some(1), "MakeLeader preserves old boss hierarchy role");

    // 7. Watchlist: AddToWatchlist, RegisterForDamageUpdates, RemoveFromWatchlist + persistence + real weapon damage streaming
    let non_member_watch = ctx.dispatch_rpc(&mut sess2, "fleetMgr", None, "AddToWatchlist", &[PyRep::List(vec![PyRep::Integer(pilot3)])], &kw, None).await.unwrap_err();
    assert_eq!(non_member_watch.code, "MemberNotFound");

    // Pre-damage Pilot 1's docked ship via persisted SHIELD_CHARGE (attr 264) to verify initial watchlist HP ratio
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(sh) = snap.items.get_mut(&ship1_id) {
            sh.dynamic_attributes.insert(264, 100.0);
        }
    });
    let _ = ctx.drain_notifications_for_char(PILOT2);
    let add_watch = ctx.dispatch_rpc(
        &mut sess2, "fleetMgr", None, "AddToWatchlist",
        &[PyRep::List(vec![PyRep::Integer(PILOT1)]), PyRep::List(vec![PyRep::Integer(PILOT1)])],
        &kw, None,
    ).await.expect("AddToWatchlist");
    assert_eq!(get_bool(&add_watch, "watched"), Some(true));
    let init_sr = add_watch.dict_get("shieldRatio").and_then(|v| v.as_f64()).expect("initial shieldRatio");
    assert!(init_sr < 1.0 && init_sr > 0.0, "Docked ship with persisted SHIELD_CHARGE must report partial shieldRatio, got {init_sr}");
    assert!(ctx.drain_notifications_for_char(PILOT2).iter().any(|n| n.event == "OnFleetStateChange"));

    // RegisterForDamageUpdates with list persists fleet_watchlist in service_state_blobs; verify SQLite roundtrip & cache eviction hydration
    let reg_dam = ctx.dispatch_rpc(&mut sess2, "fleetMgr", None, "RegisterForDamageUpdates", &[PyRep::List(vec![PyRep::Integer(PILOT1)])], &kw, None).await.expect("RegisterForDamageUpdates");
    assert_eq!(get_bool(&reg_dam, "watched"), Some(true));
    ctx.db.save_snapshot_to_file(&json_path).expect("save_snapshot_to_file watchlist");
    {
        let writer = eve_db::WorldRepository::open_sqlite(&sqlite_path).expect("open_sqlite writer watchlist");
        writer.load_snapshot_from_file(&json_path).expect("load_snapshot_from_file watchlist");
    }
    let repo_wl = eve_db::WorldRepository::open_sqlite(&sqlite_path).expect("reopen_sqlite watchlist");
    let snap_wl = repo_wl.with_snapshot(|s| s.clone());
    let _ = std::fs::remove_file(&json_path);
    let _ = std::fs::remove_file(&sqlite_path);
    ctx.db.with_mut_snapshot(|s| s.service_state_blobs = snap_wl.service_state_blobs);
    eve_server::services::fleet_social::hydrate_social_state_from_db(&ctx);
    let init_reloaded = ctx.dispatch_rpc(&mut sess2, "fleetObjectHandler", None, "GetInitState", &[], &kw, None).await.expect("GetInitState after SQLite reload");
    let opts_reloaded = init_reloaded.dict_get("options").expect("options after reload");
    assert_eq!(get_bool(opts_reloaded, "isVoiceEnabled"), Some(true));
    assert_eq!(get_bool(&init_reloaded, "isLootLogging"), Some(true));

    // Spawn watched Pilot 1 and attacker Pilot 3 in lowsec (30_002_537) and fire real weapon via dogmaIM.Activate
    let lowsec_sys = 30_002_537u32;
    let ship3_id = 400_000_303i64;
    let autocannon_id = 400_000_304i64;
    let ammo_id = 400_000_305i64;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c1) = snap.characters.get_mut(&PILOT1) {
            c1.solar_system_id = lowsec_sys;
            c1.station_id = None;
            c1.active_ship_id = ship1_id;
        }
        if let Some(s1) = snap.items.get_mut(&ship1_id) {
            s1.location_id = lowsec_sys as i64;
        }
        if let Some(c3) = snap.characters.get_mut(&pilot3) {
            c3.solar_system_id = lowsec_sys;
            c3.station_id = None;
            c3.active_ship_id = ship3_id;
        }
        snap.items.insert(ship3_id, eve_db::InventoryItemRecord {
            item_id: ship3_id, type_id: 587, owner_id: pilot3, location_id: lowsec_sys as i64,
            flag: eve_db::InventoryFlag::None, quantity: 1, singleton: true, version: 1, ..Default::default()
        });
        snap.items.insert(autocannon_id, eve_db::InventoryItemRecord {
            item_id: autocannon_id, type_id: 486, owner_id: pilot3, location_id: ship3_id,
            flag: eve_db::InventoryFlag::HiSlot0, quantity: 1, singleton: true, version: 1, ..Default::default()
        });
        snap.items.insert(ammo_id, eve_db::InventoryItemRecord {
            item_id: ammo_id, type_id: 185, owner_id: pilot3, location_id: ship3_id,
            flag: eve_db::InventoryFlag::HiSlot0, quantity: 100, singleton: false, version: 1, ..Default::default()
        });
    });
    sess1.solar_system_id = Some(lowsec_sys);
    sess1.station_id = None;
    sess1.ship_id = Some(ship1_id);
    sess3.solar_system_id = Some(lowsec_sys);
    sess3.station_id = None;
    sess3.ship_id = Some(ship3_id);
    let sys_handle = ctx.systems.get_or_spawn(lowsec_sys);
    let _ = sys_handle.spawn_ball_with_combat(
        eve_destiny::Ball {
            id: ship1_id, type_id: 587, position: eve_destiny::DVec3::ZERO, velocity: eve_destiny::DVec3::ZERO,
            max_velocity: 350.0, inertia: 3.0, mass: 1_200_000.0, radius: 40.0, speed_fraction: 0.0, mode: eve_destiny::MovementMode::Stop,
        },
        ctx.build_type_combat_state(587),
    ).await;
    let _ = sys_handle.spawn_ball_with_combat(
        eve_destiny::Ball {
            id: ship3_id, type_id: 587, position: eve_destiny::DVec3::new(1_000.0, 0.0, 0.0), velocity: eve_destiny::DVec3::ZERO,
            max_velocity: 350.0, inertia: 3.0, mass: 1_200_000.0, radius: 40.0, speed_fraction: 0.0, mode: eve_destiny::MovementMode::Stop,
        },
        ctx.build_type_combat_state(587),
    ).await;

    let _ = ctx.drain_notifications_for_char(PILOT2);
    ctx.set_invention_roll_override(Some(0.50));
    eve_server::services::combat_crimewatch::dispatch_combat_rpc(
        &ctx,
        &mut sess3,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(autocannon_id), PyRep::Integer(ship1_id)],
        &kw,
    )
    .await
    .expect("dogmaIM.Activate real weapon damage against watched Pilot 1");

    let mut notifs = ctx.drain_notifications_for_char(PILOT2);
    assert!(
        notifs.iter().any(|n| n.event == "OnDamageStateChange" && n.args.first() == Some(&PyRep::Integer(ship1_id))),
        "Watcher Pilot 2 must receive OnDamageStateChange for watched ship"
    );
    let dam_notif = notifs.into_iter().find(|n| n.event == "OnFleetStateChange").expect("Pilot 2 must receive OnFleetStateChange on combat damage");
    assert_eq!(dam_notif.args.len(), 1, "OnFleetStateChange must carry 1 argument (fleetState KeyVal)");
    let fleet_state = &dam_notif.args[0];
    assert_eq!(get_i64(fleet_state, "characterID"), Some(PILOT1));
    assert!(matches!(fleet_state.dict_get("targetTags"), Some(PyRep::Dict(_))));
    let shield_ratio = fleet_state.dict_get("shieldRatio").and_then(|v| v.as_f64()).expect("shieldRatio");
    assert!(shield_ratio < 1.0, "Real weapon damage must reduce watched Pilot 1 shieldRatio below 1.0, got {shield_ratio}");

    // RemoveFromWatchlist
    let rem_watch = ctx
        .dispatch_rpc(
            &mut sess2,
            "fleetMgr",
            None,
            "RemoveFromWatchlist",
            &[PyRep::Integer(PILOT1)],
            &kw,
            None,
        )
        .await
        .expect("RemoveFromWatchlist");
    assert_eq!(get_bool(&rem_watch, "watched"), Some(false));

    // Fire weapon again -> Pilot 2 receives NO damage updates after RemoveFromWatchlist
    let _ = ctx.drain_notifications_for_char(PILOT2);
    eve_server::services::combat_crimewatch::dispatch_combat_rpc(
        &ctx,
        &mut sess3,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(autocannon_id), PyRep::Integer(ship1_id)],
        &kw,
    )
    .await
    .expect("dogmaIM.Activate second shot");
    notifs = ctx.drain_notifications_for_char(PILOT2);
    assert!(!notifs.into_iter().any(|n| n.event == "OnFleetStateChange"), "Removed watcher must not receive updates");

    // 8. CreateFleet when already in a fleet returns AlreadyInFleet
    let create2_err = ctx
        .dispatch_rpc(
            &mut sess1,
            "fleetObjectHandler",
            None,
            "CreateFleet",
            &[PyRep::String("Beta Fleet".into())],
            &kw,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(create2_err.code, "AlreadyInFleet");
}
