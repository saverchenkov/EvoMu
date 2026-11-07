//! Integration tests for PVE Agent Finder, Conversation, and Full Mission Lifecycle (`E-5`).
//!
//! Covers parity rows:
//! - `feat:pve.agent-finder`: Agent finder queries filtered by corporation, faction, level, security status, and agent type.
//! - `feat:pve.agent-conversation`: Agent interaction dialog, greeting, mission briefing, offer details (`agentMgr.GetOffer` / `DoAction`).
//! - `feat:pve.mission-accept`: Mission acceptance lifecycle state transition from Offered to Accepted.
//! - `feat:pve.mission-decline`: Mission decline mechanics, cooldowns, and standings impact.
//! - `feat:pve.mission-quit`: Mission abort / quit mechanics and failure state.
//! - `feat:pve.mission-complete`: Mission objective completion, ISK/LP reward payout, bonus time rewards, and turn-in.
//! - `feat:pve.mission-expire`: Offered mission expiration timeout and cleanup.
//! - `feat:pve.mission-journal`: Character active and completed mission journal entries and bookmarks.
//! - `feat:pve.mission-tracker`: HUD mission tracker objective tracking, dungeon room progression, and NPC kill counts.

#![forbid(unsafe_code)]

use eve_db::{BookmarkRecord, IskCents};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use std::collections::HashMap;

async fn create_auth_session(ctx: &ServerContext, session_id: u64, char_id: i64) -> SessionState {
    let kw = HashMap::new();
    let mut sess = SessionState::new(session_id);
    ctx.dispatch_rpc(&mut sess, "authentication", None, "Login", &[PyRep::String("demo".into()), PyRep::String("demo-secret".into())], &kw, None).await.expect("Login failed");
    ctx.dispatch_rpc(&mut sess, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(char_id)], &kw, None).await.expect("SelectCharacterID failed");
    sess.solar_system_id = Some(30_000_142);
    sess.station_id = Some(60_003_760);
    sess.region_id = Some(10_000_002);
    sess
}

async fn bind_service_oid(ctx: &ServerContext, sess: &mut SessionState, service: &str, bind_param: PyRep) -> u64 {
    let kw = HashMap::new();
    let res = ctx.dispatch_rpc(sess, service, None, "MachoBindObject", &[bind_param, PyRep::None], &kw, None).await.expect("MachoBindObject");
    match res {
        PyRep::Tuple(outer) => outer[0].as_i64().expect("bound_id integer") as u64,
        PyRep::Integer(oid) => oid as u64,
        _ => panic!("Expected Tuple or integer from MachoBindObject, got {res:?}"),
    }
}

#[tokio::test]
async fn test_pve_agent_finder_and_conversation() {
    // parity: feat:pve.agent-finder, feat:pve.agent-conversation
    eve_testkit::attest(&["feat:pve.agent-finder", "feat:pve.agent-conversation"]);
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let char_id = 90_000_001_i64;
    let mut sess = create_auth_session(&ctx, 2001, char_id).await;
    let kw = HashMap::new();

    // 1. Agent Finder queries (feat:pve.agent-finder)
    // 1a. Query Level 1 Security (division 24) agents
    let l1_rep = ctx.dispatch_rpc(&mut sess, "agentMgr", None, "GetAgents", &[PyRep::Integer(1), PyRep::Integer(24)], &kw, None).await.expect("GetAgents L1");
    let PyRep::Rowset { columns: agents_cols, rows: l1_rows } = l1_rep else { panic!("Expected Rowset from GetAgents") };
    assert_eq!(agents_cols, vec!["agentID", "agentTypeID", "divisionID", "corporationID", "factionID", "solarsystemID", "stationID", "level", "quality", "hasAccess"]);
    assert!(!l1_rows.is_empty(), "Level 1 Security agents must be found");
    for row in &l1_rows {
        assert_eq!(row[7].as_i64(), Some(1), "Agent level must be 1");
        assert_eq!(row[2].as_i64(), Some(24), "Division must be 24");
        assert_eq!(row[9], PyRep::Bool(true), "L1 agents must have access by default");
    }

    // 1b. Filter by corporation
    let test_corp_id = l1_rows[0][3].as_i64().expect("corp id");
    let mut corp_kw = HashMap::new();
    corp_kw.insert("corporationID".to_string(), PyRep::Integer(test_corp_id));
    let corp_rep = ctx.dispatch_rpc(&mut sess, "agentMgr", None, "GetAgents", &[], &corp_kw, None).await.expect("GetAgents corp");
    let PyRep::Rowset { rows: corp_rows, .. } = corp_rep else { panic!() };
    assert!(!corp_rows.is_empty());
    for row in &corp_rows {
        assert_eq!(row[3].as_i64(), Some(test_corp_id), "Must match filtered corporation");
    }

    // 1c. Filter by faction
    let test_faction_id = l1_rows.iter().find_map(|r| r[4].as_i64()).unwrap_or(500_001);
    let mut fac_kw = HashMap::new();
    fac_kw.insert("factionID".to_string(), PyRep::Integer(test_faction_id));
    let fac_rep = ctx.dispatch_rpc(&mut sess, "agentMgr", None, "GetAgents", &[], &fac_kw, None).await.expect("GetAgents faction");
    let PyRep::Rowset { rows: fac_rows, .. } = fac_rep else { panic!() };
    assert!(!fac_rows.is_empty());
    for row in &fac_rows {
        assert_eq!(row[4].as_i64(), Some(test_faction_id), "Must match filtered faction");
    }

    // 1d. Filter by agent type (e.g. type 2 = Basic Agent)
    let mut type_kw = HashMap::new();
    type_kw.insert("agentTypeID".to_string(), PyRep::Integer(2));
    let type_rep = ctx.dispatch_rpc(&mut sess, "agentMgr", None, "GetAgents", &[], &type_kw, None).await.expect("GetAgents agentType");
    let PyRep::Rowset { rows: type_rows, .. } = type_rep else { panic!() };
    assert!(!type_rows.is_empty());
    for row in &type_rows {
        assert_eq!(row[1].as_i64(), Some(2), "Must match filtered agent type");
    }

    // 1e. Filter by security status ("highsec")
    let mut sec_kw = HashMap::new();
    sec_kw.insert("securityStatus".to_string(), PyRep::String("highsec".into()));
    let sec_rep = ctx.dispatch_rpc(&mut sess, "agentMgr", None, "GetAgents", &[], &sec_kw, None).await.expect("GetAgents highsec");
    let PyRep::Rowset { rows: sec_rows, .. } = sec_rep else { panic!() };
    assert!(!sec_rows.is_empty());
    for row in &sec_rows {
        if let Some(sys_id) = row[5].as_i64().map(|v| v as u32) {
            let sys_sec = ctx.sde.solar_systems.get(&sys_id).map(|s| s.security_status).unwrap_or(1.0);
            assert!(sys_sec >= 0.45, "Highsec filter must yield system security >= 0.45");
        }
    }

    // 1f. Gated access check on Level 2 agents (requires standing >= 1.0)
    let l2_rep = ctx.dispatch_rpc(&mut sess, "agentMgr", None, "GetAgents", &[PyRep::Integer(2), PyRep::Integer(24)], &kw, None).await.expect("GetAgents L2");
    let PyRep::Rowset { rows: l2_rows, .. } = l2_rep else { panic!() };
    assert!(!l2_rows.is_empty());
    let test_l2_agent = &l2_rows[0];
    let test_l2_agent_id = test_l2_agent[0].as_i64().unwrap();
    let test_l2_corp_id = test_l2_agent[3].as_i64().unwrap();
    assert_eq!(test_l2_agent[9], PyRep::Bool(false), "L2 agent inaccessible with 0.0 standing");

    // Boost corp standing to 3.0 and verify access unlocks
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.standings.insert(test_l2_corp_id, 3.0);
        }
    });
    let l2_st_rep = ctx.dispatch_rpc(&mut sess, "agentMgr", None, "GetAgents", &[PyRep::Integer(2), PyRep::Integer(24)], &kw, None).await.expect("GetAgents L2 after standing");
    let PyRep::Rowset { rows: l2_st_rows, .. } = l2_st_rep else { panic!() };
    let unlocked = l2_st_rows.iter().find(|r| r[0].as_i64() == Some(test_l2_agent_id)).expect("Found agent");
    assert_eq!(unlocked[9], PyRep::Bool(true), "L2 agent unlocked with standing >= 1.0");

    // 2. Agent Conversation (feat:pve.agent-conversation)
    let target_agent = l1_rows.iter().find(|r| r[5].as_i64() == Some(30_000_142)).unwrap_or(&l1_rows[0]);
    let target_agent_id = target_agent[0].as_i64().unwrap();
    let target_sys = target_agent[5].as_i64().unwrap() as u32;
    let target_station = target_agent[6].as_i64();

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = target_sys;
            ch.station_id = target_station;
        }
    });
    sess.solar_system_id = Some(target_sys);
    sess.station_id = target_station;

    let agent_oid = bind_service_oid(&ctx, &mut sess, "agentMgr", PyRep::Integer(target_agent_id)).await;

    // 2a. Query agent details and system
    let details = ctx.dispatch_rpc(&mut sess, "agentMgr", Some(agent_oid), "GetInfoServiceDetails", &[], &kw, None).await.expect("GetInfoServiceDetails");
    assert_eq!(details.dict_get("agentID").and_then(|v| v.as_i64()), Some(target_agent_id));
    assert_eq!(details.dict_get("level").and_then(|v| v.as_i64()), Some(1));
    if let Some(PyRep::List(services)) = details.dict_get("services") {
        assert!(services.iter().any(|s| s.as_str() == Some("mission")));
    } else {
        panic!("Expected services list");
    }

    let sys_rep = ctx.dispatch_rpc(&mut sess, "agentMgr", Some(agent_oid), "GetSolarSystemOfAgent", &[], &kw, None).await.expect("GetSolarSystemOfAgent");
    assert_eq!(sys_rep.as_i64(), Some(target_sys as i64));

    // 2b. Request mission offer via DoAction(2)
    let offer_res = ctx.dispatch_rpc(&mut sess, "agentMgr", Some(agent_oid), "DoAction", &[PyRep::Integer(2)], &kw, None).await.expect("DoAction(RequestMission)");
    let PyRep::KeyVal(offer_kv) = offer_res else { panic!("Expected KeyVal offer") };
    let mission_id = offer_kv["missionID"].as_i64().expect("missionID");
    assert_eq!(offer_kv["agentID"].as_i64(), Some(target_agent_id));
    assert_eq!(offer_kv["state"].as_str(), Some("Offered"));
    assert_eq!(offer_kv["missionState"].as_i64(), Some(1));
    assert!(offer_kv["briefing"].as_str().is_some_and(|s| !s.is_empty()));
    assert!(offer_kv["objectives"].as_str().is_some_and(|s| !s.is_empty()));
    assert!(offer_kv["iskReward"].as_i64().unwrap_or(0) > 0);
    assert!(offer_kv["lpReward"].as_i64().unwrap_or(0) > 0);
    assert!(offer_kv["bonusReward"].as_i64().unwrap_or(0) > 0);
    assert!(offer_kv["expiryUnix"].as_i64().unwrap_or(0) > 0);

    // 2c. GetMissionBriefingInfo
    let briefing_info = ctx.dispatch_rpc(&mut sess, "agentMgr", Some(agent_oid), "GetMissionBriefingInfo", &[], &kw, None).await.expect("GetMissionBriefingInfo");
    assert_eq!(briefing_info.dict_get("missionID").and_then(|v| v.as_i64()), Some(mission_id));
}

#[tokio::test]
async fn test_pve_mission_lifecycle_accept_tracker_complete_journal() {
    // parity: feat:pve.mission-accept, feat:pve.mission-journal, feat:pve.mission-tracker
    eve_testkit::attest(&["feat:pve.mission-accept", "feat:pve.mission-journal", "feat:pve.mission-tracker"]);
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let char_id = 90_000_001_i64;
    let mut sess = create_auth_session(&ctx, 2002, char_id).await;
    let kw = HashMap::new();

    let l1_rep = ctx.dispatch_rpc(&mut sess, "agentMgr", None, "GetAgents", &[PyRep::Integer(1), PyRep::Integer(24)], &kw, None).await.expect("GetAgents");
    let PyRep::Rowset { rows: l1_rows, .. } = l1_rep else { panic!() };
    let agent_row = l1_rows.iter().find(|r| r[5].as_i64() == Some(30_000_142)).unwrap_or(&l1_rows[0]);
    let agent_id = agent_row[0].as_i64().unwrap();
    let corp_id = agent_row[3].as_i64().unwrap();
    let sys_id = agent_row[5].as_i64().unwrap() as u32;
    let station_id = agent_row[6].as_i64();

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = sys_id;
            ch.station_id = station_id;
            ch.balance = IskCents::from_isk(10_000_000);
            ch.standings.insert(corp_id, 0.0);
            ch.loyalty_points.insert(corp_id, 100);
        }
    });
    sess.solar_system_id = Some(sys_id);
    sess.station_id = station_id;

    let agent_oid = bind_service_oid(&ctx, &mut sess, "agentMgr", PyRep::Integer(agent_id)).await;

    // 1. Request mission offer
    let offer = ctx.dispatch_rpc(&mut sess, "agentMgr", Some(agent_oid), "DoAction", &[PyRep::Integer(2)], &kw, None).await.expect("DoAction(Request)");
    let PyRep::KeyVal(offer_kv) = offer else { panic!() };
    let mission_id = offer_kv["missionID"].as_i64().unwrap() as u32;
    let exp_isk = offer_kv["iskReward"].as_i64().unwrap();
    let exp_bonus = offer_kv["bonusReward"].as_i64().unwrap();
    let exp_lp = offer_kv["lpReward"].as_i64().unwrap();
    assert!(exp_isk > 0 && exp_bonus > 0 && exp_lp > 0, "Offer must specify positive rewards");

    // 2. Accept mission (feat:pve.mission-accept)
    let accept_rep = ctx.dispatch_rpc(&mut sess, "agentMgr", Some(agent_oid), "DoAction", &[PyRep::Integer(3)], &kw, None).await.expect("DoAction(Accept)");
    let PyRep::KeyVal(acc_kv) = accept_rep else { panic!() };
    assert_eq!(acc_kv["missionID"].as_i64(), Some(mission_id as i64));
    assert_eq!(acc_kv["state"].as_str(), Some("Accepted"));
    assert_eq!(acc_kv["missionState"].as_i64(), Some(2));
    assert_eq!(acc_kv["currentRoomID"].as_i64(), Some(1));
    let bookmark_id = acc_kv["bookmarkID"].as_i64().expect("bookmarkID");
    assert!(bookmark_id > 0);
    assert!(acc_kv["expiryUnix"].as_i64().unwrap() > ctx.now_unix() as i64);
    assert!(acc_kv["bonusExpiryUnix"].as_i64().unwrap() > ctx.now_unix() as i64);

    let bm_record = ctx.db.with_snapshot(|snap| snap.bookmarks.get(&bookmark_id).cloned());
    assert!(bm_record.is_some(), "Mission bookmark must exist in DB");
    assert!(bm_record.unwrap().memo.contains("Mission Pocket Destination"));

    // 3. Verify Journal entry (feat:pve.mission-journal)
    let journal = ctx.dispatch_rpc(&mut sess, "agentMgr", None, "GetMyJournalDetails", &[], &kw, None).await.expect("GetMyJournalDetails");
    let PyRep::Tuple(j_sections) = journal else { panic!() };
    let PyRep::List(active_missions) = &j_sections[0] else { panic!() };
    let j_entry = active_missions.iter().find(|m| m.dict_get("missionID").and_then(|v| v.as_i64()) == Some(mission_id as i64)).expect("Mission must be present in journal");
    assert_eq!(j_entry.dict_get("state").and_then(|v| v.as_str()), Some("Accepted"));
    assert_eq!(j_entry.dict_get("missionState").and_then(|v| v.as_i64()), Some(2));
    assert_eq!(j_entry.dict_get("level").and_then(|v| v.as_i64()), Some(1));
    if let Some(PyRep::List(bms)) = j_entry.dict_get("bookmarks") {
        assert!(bms.iter().any(|b| b.dict_get("bookmarkID").and_then(|v| v.as_i64()) == Some(bookmark_id)));
    } else {
        panic!("Expected bookmarks list in journal entry");
    }

    // 4. Mission Tracker objective tracking (feat:pve.mission-tracker)
    let tracker_objs = ctx.dispatch_rpc(&mut sess, "missionTrackerMgr", None, "GetAllMissionObjectives", &[], &kw, None).await.expect("GetAllMissionObjectives");
    let PyRep::List(obj_list) = tracker_objs else { panic!() };
    let trk = obj_list.iter().find(|o| o.dict_get("missionID").and_then(|v| v.as_i64()) == Some(mission_id as i64)).expect("Mission must be in tracker list");
    assert_eq!(trk.dict_get("state").and_then(|v| v.as_str()), Some("Accepted"));
    assert_eq!(trk.dict_get("currentRoomID").and_then(|v| v.as_i64()), Some(1));
    assert_eq!(trk.dict_get("objectivesCompleted"), Some(&PyRep::Bool(false)));

    let update_rep = ctx.dispatch_rpc(&mut sess, "missionTrackerMgr", None, "UpdateAllMissions", &[], &kw, None).await.expect("UpdateAllMissions");
    assert!(matches!(update_rep, PyRep::List(_)));

    let in_dungeon = ctx.dispatch_rpc(&mut sess, "missionTrackerMgr", None, "IsInActiveDungeonID", &[], &kw, None).await.expect("IsInActiveDungeonID");
    assert_eq!(in_dungeon, PyRep::Bool(true));

    // Progress mission: defeat NPCs
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(m) = snap.active_missions.get_mut(&(char_id, mission_id)) {
            m.npcs_remaining = 0;
        }
    });

    let tracker_done = ctx.dispatch_rpc(&mut sess, "missionTrackerMgr", None, "GetAllMissionObjectives", &[], &kw, None).await.expect("GetAllMissionObjectives after kills");
    let PyRep::List(done_list) = tracker_done else { panic!() };
    let trk_done = done_list.iter().find(|o| o.dict_get("missionID").and_then(|v| v.as_i64()) == Some(mission_id as i64)).unwrap();
    assert_eq!(trk_done.dict_get("objectivesCompleted"), Some(&PyRep::Bool(true)));
    assert_eq!(trk_done.dict_get("npcsRemaining").and_then(|v| v.as_i64()), Some(0));

    // 5. Complete mission
    let initial_isk = ctx.db.with_snapshot(|snap| snap.characters[&char_id].balance.0 / 100);
    let initial_lp = ctx.db.with_snapshot(|snap| snap.characters[&char_id].loyalty_points.get(&corp_id).copied().unwrap_or(0));
    let initial_standing = ctx.db.with_snapshot(|snap| snap.characters[&char_id].standings.get(&corp_id).copied().unwrap_or(0.0));

    let complete_rep = ctx.dispatch_rpc(&mut sess, "agentMgr", Some(agent_oid), "DoAction", &[PyRep::Integer(6)], &kw, None).await.expect("DoAction(Complete)");
    let PyRep::KeyVal(comp_kv) = complete_rep else { panic!() };
    assert_eq!(comp_kv["missionID"].as_i64(), Some(mission_id as i64));
    assert_eq!(comp_kv["state"].as_str(), Some("Completed"));
    assert_eq!(comp_kv["missionState"].as_i64(), Some(4));
    let paid_isk = comp_kv["iskReward"].as_i64().unwrap();
    let paid_bonus = comp_kv["bonusReward"].as_i64().unwrap();
    let paid_lp = comp_kv["lpReward"].as_i64().unwrap();
    assert!(paid_isk > 0, "ISK reward must be paid");
    assert!(paid_bonus > 0, "Time bonus reward must be paid");
    assert!(paid_lp > 0, "LP reward must be paid");
    assert_eq!(comp_kv["missionTally"].as_i64(), Some(1));
    let new_st = comp_kv["newStanding"].as_f64().expect("newStanding");
    assert!(new_st > initial_standing, "Standing must increase on completion");

    let post_isk = ctx.db.with_snapshot(|snap| snap.characters[&char_id].balance.0 / 100);
    let post_lp = ctx.db.with_snapshot(|snap| snap.characters[&char_id].loyalty_points.get(&corp_id).copied().unwrap_or(0));
    assert!(post_isk > initial_isk, "Balance must increase after completion");
    assert!(post_isk <= initial_isk + paid_isk + paid_bonus, "Net increase cannot exceed gross rewards after corp tax");
    let net_paid_lp = if paid_isk >= 100_000 {
        paid_lp - ((paid_lp * 11 + 50) / 100)
    } else {
        paid_lp
    };
    assert_eq!(post_lp, initial_lp + net_paid_lp);

    let journal_entries = ctx.db.with_snapshot(|snap| {
        snap.wallet_journal.iter().filter(|e| e.character_id == char_id).map(|e| e.entry_type.clone()).collect::<Vec<_>>()
    });
    assert!(journal_entries.contains(&"agent_mission_reward".to_string()));
    assert!(journal_entries.contains(&"agent_mission_time_bonus_reward".to_string()));

    let post_journal = ctx.dispatch_rpc(&mut sess, "agentMgr", None, "GetMyJournalDetails", &[], &kw, None).await.expect("GetMyJournalDetails after completion");
    let PyRep::Tuple(post_tuple) = post_journal else { panic!() };
    let PyRep::List(post_missions) = &post_tuple[0] else { panic!() };
    assert!(!post_missions.iter().any(|m| m.dict_get("missionID").and_then(|v| v.as_i64()) == Some(mission_id as i64)));
}

#[tokio::test]
async fn test_pve_mission_decline_quit_and_expiration() {
    // parity: feat:pve.mission-decline, feat:pve.mission-quit, feat:pve.mission-expire
    eve_testkit::attest(&["feat:pve.mission-decline", "feat:pve.mission-quit", "feat:pve.mission-expire"]);
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let char_id = 90_000_001_i64;
    let mut sess = create_auth_session(&ctx, 2003, char_id).await;
    let kw = HashMap::new();

    let l1_rep = ctx.dispatch_rpc(&mut sess, "agentMgr", None, "GetAgents", &[PyRep::Integer(1), PyRep::Integer(24)], &kw, None).await.expect("GetAgents");
    let PyRep::Rowset { rows: l1_rows, .. } = l1_rep else { panic!() };
    let agent_row = l1_rows.iter().find(|r| r[5].as_i64() == Some(30_000_142)).unwrap_or(&l1_rows[0]);
    let agent_id = agent_row[0].as_i64().unwrap();
    let corp_id = agent_row[3].as_i64().unwrap();
    let sys_id = agent_row[5].as_i64().unwrap() as u32;
    let station_id = agent_row[6].as_i64();

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = sys_id;
            ch.station_id = station_id;
            ch.standings.insert(corp_id, 2.0);
            ch.standings.insert(agent_id, 2.0);
        }
    });
    sess.solar_system_id = Some(sys_id);
    sess.station_id = station_id;

    let agent_oid = bind_service_oid(&ctx, &mut sess, "agentMgr", PyRep::Integer(agent_id)).await;

    // 1. Mission Decline (feat:pve.mission-decline)
    // 1a. First decline: no standing loss within cooldown
    let offer_1 = ctx.dispatch_rpc(&mut sess, "agentMgr", Some(agent_oid), "DoAction", &[PyRep::Integer(2)], &kw, None).await.expect("Offer 1");
    let PyRep::KeyVal(o1_kv) = offer_1 else { panic!() };
    let mid_1 = o1_kv["missionID"].as_i64().unwrap() as u32;

    let standing_before_dec1 = ctx.db.with_snapshot(|snap| snap.characters[&char_id].standings[&corp_id]);
    let dec_1 = ctx.dispatch_rpc(&mut sess, "agentMgr", Some(agent_oid), "DoAction", &[PyRep::Integer(9)], &kw, None).await.expect("Decline 1");
    let PyRep::KeyVal(dec1_kv) = dec_1 else { panic!() };
    assert_eq!(dec1_kv["missionID"].as_i64(), Some(mid_1 as i64));
    assert_eq!(dec1_kv["state"].as_str(), Some("Declined"));
    assert_eq!(dec1_kv["missionState"].as_i64(), Some(5));
    assert_eq!(dec1_kv["standingPenaltyApplied"], PyRep::Bool(false));

    let standing_after_dec1 = ctx.db.with_snapshot(|snap| snap.characters[&char_id].standings[&corp_id]);
    assert_eq!(standing_after_dec1, standing_before_dec1, "First decline must incur no standing penalty");

    // 1b. Second decline within 4h cooldown: incurs standing loss
    let offer_2 = ctx.dispatch_rpc(&mut sess, "agentMgr", Some(agent_oid), "DoAction", &[PyRep::Integer(2)], &kw, None).await.expect("Offer 2");
    let PyRep::KeyVal(o2_kv) = offer_2 else { panic!() };
    let mid_2 = o2_kv["missionID"].as_i64().unwrap() as u32;

    let dec_2 = ctx.dispatch_rpc(&mut sess, "agentMgr", Some(agent_oid), "DoAction", &[PyRep::Integer(9)], &kw, None).await.expect("Decline 2");
    let PyRep::KeyVal(dec2_kv) = dec_2 else { panic!() };
    assert_eq!(dec2_kv["missionID"].as_i64(), Some(mid_2 as i64));
    assert_eq!(dec2_kv["standingPenaltyApplied"], PyRep::Bool(true));

    let standing_after_dec2 = ctx.db.with_snapshot(|snap| snap.characters[&char_id].standings[&corp_id]);
    assert!(standing_after_dec2 < standing_after_dec1, "Repeated decline within 4h must penalize standing");

    // 2. Mission Quit / Abort (feat:pve.mission-quit)
    let offer_3 = ctx.dispatch_rpc(&mut sess, "agentMgr", Some(agent_oid), "DoAction", &[PyRep::Integer(2)], &kw, None).await.expect("Offer 3");
    let PyRep::KeyVal(o3_kv) = offer_3 else { panic!() };
    let mid_3 = o3_kv["missionID"].as_i64().unwrap() as u32;

    let acc_3 = ctx.dispatch_rpc(&mut sess, "agentMgr", Some(agent_oid), "DoAction", &[PyRep::Integer(3)], &kw, None).await.expect("Accept 3");
    let PyRep::KeyVal(acc3_kv) = acc_3 else { panic!() };
    let bm_id = acc3_kv["bookmarkID"].as_i64().unwrap();
    assert!(ctx.db.with_snapshot(|snap| snap.bookmarks.contains_key(&bm_id)));

    let personal_bm_id = 999_881_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.bookmarks.insert(
            personal_bm_id,
            BookmarkRecord {
                bookmark_id: personal_bm_id,
                owner_id: char_id,
                solar_system_id: sys_id,
                label: "Personal Safe".into(),
                x: 100.0,
                y: 200.0,
                z: 300.0,
                item_id: None,
                memo: "Personal Spot".into(),
                folder_id: None,
                is_shared: false,
            },
        );
    });

    let standing_before_quit = ctx.db.with_snapshot(|snap| snap.characters[&char_id].standings[&corp_id]);
    let quit_rep = ctx.dispatch_rpc(&mut sess, "agentMgr", Some(agent_oid), "DoAction", &[PyRep::Integer(11)], &kw, None).await.expect("DoAction(Quit)");
    let PyRep::KeyVal(quit_kv) = quit_rep else { panic!() };
    assert_eq!(quit_kv["missionID"].as_i64(), Some(mid_3 as i64));
    assert_eq!(quit_kv["state"].as_str(), Some("Quit"));
    assert_eq!(quit_kv["missionState"].as_i64(), Some(6));
    assert_eq!(quit_kv["standingPenaltyApplied"], PyRep::Bool(true));
    assert_eq!(quit_kv["dungeonCleaned"], PyRep::Bool(true));

    let standing_after_quit = ctx.db.with_snapshot(|snap| snap.characters[&char_id].standings[&corp_id]);
    assert!(standing_after_quit < standing_before_quit, "Quitting accepted mission must penalize standing");
    assert!(!ctx.db.with_snapshot(|snap| snap.bookmarks.contains_key(&bm_id)), "Mission bookmark must be removed");
    assert!(ctx.db.with_snapshot(|snap| snap.bookmarks.contains_key(&personal_bm_id)), "Personal bookmark must persist");

    let jq_rep = ctx.dispatch_rpc(&mut sess, "agentMgr", None, "GetMyJournalDetails", &[], &kw, None).await.expect("Journal");
    let PyRep::Tuple(jq_tup) = jq_rep else { panic!() };
    let PyRep::List(jq_list) = &jq_tup[0] else { panic!() };
    assert!(jq_list.is_empty(), "Journal active missions must be empty after quit");

    // 3. Mission Expiration (feat:pve.mission-expire)
    // 3a. Unaccepted offer expiration
    let offer_4 = ctx.dispatch_rpc(&mut sess, "agentMgr", Some(agent_oid), "DoAction", &[PyRep::Integer(2)], &kw, None).await.expect("Offer 4");
    let PyRep::KeyVal(o4_kv) = offer_4 else { panic!() };
    let mid_4 = o4_kv["missionID"].as_i64().unwrap() as u32;

    ctx.db.with_mut_snapshot(|snap| {
        for note in snap.owner_notes.values_mut() {
            if note.item_id == mid_4 as i64 && note.note.contains("offer_expiry_unix") {
                if let Ok(mut val) = serde_json::from_str::<serde_json::Value>(&note.note) {
                    val["offer_expiry_unix"] = serde_json::json!(500_u64);
                    note.note = val.to_string();
                }
            }
        }
    });

    let accept_expired = ctx.dispatch_rpc(&mut sess, "agentMgr", Some(agent_oid), "DoAction", &[PyRep::Integer(3)], &kw, None).await.expect_err("Expired offer must reject accept");
    assert_eq!(accept_expired.code, "MissionOfferExpired");

    let j_exp = ctx.dispatch_rpc(&mut sess, "agentMgr", None, "GetMyJournalDetails", &[], &kw, None).await.expect("Journal sweep");
    let PyRep::Tuple(je_tup) = j_exp else { panic!() };
    let PyRep::List(je_list) = &je_tup[0] else { panic!() };
    assert!(!je_list.iter().any(|m| m.dict_get("missionID").and_then(|v| v.as_i64()) == Some(mid_4 as i64)));
    assert_eq!(ctx.db.with_snapshot(|snap| snap.active_missions[&(char_id, mid_4)].state.clone()), "Expired");

    // 3b. Accepted mission deadline expiration
    let offer_5 = ctx.dispatch_rpc(&mut sess, "agentMgr", Some(agent_oid), "DoAction", &[PyRep::Integer(2)], &kw, None).await.expect("Offer 5");
    let PyRep::KeyVal(o5_kv) = offer_5 else { panic!() };
    let mid_5 = o5_kv["missionID"].as_i64().unwrap() as u32;

    ctx.dispatch_rpc(&mut sess, "agentMgr", Some(agent_oid), "DoAction", &[PyRep::Integer(3)], &kw, None).await.expect("Accept 5");

    ctx.db.with_mut_snapshot(|snap| {
        for note in snap.owner_notes.values_mut() {
            if note.item_id == mid_5 as i64 && note.note.contains("accept_expiry_unix") {
                if let Ok(mut val) = serde_json::from_str::<serde_json::Value>(&note.note) {
                    val["accept_expiry_unix"] = serde_json::json!(500_u64);
                    note.note = val.to_string();
                }
            }
        }
    });

    let comp_expired = ctx.dispatch_rpc(&mut sess, "agentMgr", Some(agent_oid), "DoAction", &[PyRep::Integer(6)], &kw, None).await.expect_err("Completing expired mission must fail");
    assert_eq!(comp_expired.code, "MissionExpired");
    assert_eq!(ctx.db.with_snapshot(|snap| snap.active_missions[&(char_id, mid_5)].state.clone()), "Failed");
}

async fn dock_pilot_at_station(
    ctx: &ServerContext,
    sess: &mut SessionState,
    char_id: i64,
    ship_id: i64,
    sys_id: u32,
    station_id: i64,
) {
    let kw = HashMap::new();
    let cur_station = ctx.db.with_snapshot(|s| s.characters[&char_id].station_id);
    if cur_station == Some(station_id) {
        return;
    }
    if cur_station.is_some() {
        crate::common::slash_cmd(ctx, sess, "/giveskill 3332 5").await;
        ctx.advance_secs(31);
        ctx.dispatch_rpc(sess, "stationSvc", None, "Undock", &[], &kw, None).await.expect("Undock");
    }
    if sess.solar_system_id != Some(sys_id) {
        crate::common::slash_cmd(ctx, sess, &format!("/tr {sys_id}")).await;
    }
    if let Some(st) = ctx.sde.stations.get(&station_id) {
        let sys = ctx.systems.get_or_spawn(sys_id);
        sys.set_ball_position(ship_id, st.position + glam::DVec3::new(500.0, 0.0, 0.0)).await;
        sys.step_manual(0.1).await.unwrap();
    }
    ctx.dispatch_rpc(sess, "stationSvc", None, "Dock", &[PyRep::Integer(station_id)], &kw, None).await.expect("Dock");
}

#[tokio::test]
async fn test_live_plan_e5_agent_missions_end_to_end() {
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let char_id = 96_005_001_i64;
    ctx.db.provision_gm_account("e5pilot", "e5-secret", char_id, "E5 Mission Pilot");
    let mut sess = SessionState::new(9605);
    crate::common::login_pilot(&ctx, &mut sess, "e5pilot", "e5-secret").await;
    let kw = HashMap::new();
    ctx.dispatch_rpc(&mut sess, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(char_id)], &kw, None).await.expect("SelectCharacterID");

    let ship_id = ctx.db.with_snapshot(|s| s.characters[&char_id].active_ship_id);

    // 1. Level 2 agent rejects character at 0.0 standing
    let l2_agent = ctx.sde.agents.values()
        .find(|a| a.level == 2 && a.division_id == 24 && a.agent_type_id == 2 && a.station_id.is_some() && a.solar_system_id.is_some())
        .expect("L2 Security agent")
        .clone();
    dock_pilot_at_station(&ctx, &mut sess, char_id, ship_id, l2_agent.solar_system_id.unwrap(), l2_agent.station_id.unwrap()).await;
    let l2_oid = bind_service_oid(&ctx, &mut sess, "agentMgr", PyRep::Integer(l2_agent.agent_id)).await;
    let gate_err = ctx.dispatch_rpc(&mut sess, "agentMgr", Some(l2_oid), "DoAction", &[PyRep::Integer(2)], &kw, None).await.unwrap_err();
    assert_eq!(gate_err.code, "InsufficientAgentStanding");

    // 2. Security encounter mission: skill scaling, ambient NPC non-interference, spawned NPC kills, and standing propagation
    let sec_agent = ctx.sde.agents.values()
        .find(|a| a.level == 1 && a.division_id == 24 && a.agent_type_id == 2 && a.station_id.is_some() && a.solar_system_id.is_some())
        .expect("L1 Security agent")
        .clone();
    let sec_sys = sec_agent.solar_system_id.unwrap();
    let sec_sta = sec_agent.station_id.unwrap();
    dock_pilot_at_station(&ctx, &mut sess, char_id, ship_id, sec_sys, sec_sta).await;
    let sec_oid = bind_service_oid(&ctx, &mut sess, "agentMgr", PyRep::Integer(sec_agent.agent_id)).await;

    let offer_unskilled = ctx.dispatch_rpc(&mut sess, "agentMgr", Some(sec_oid), "DoAction", &[PyRep::Integer(2)], &kw, None).await.expect("Unskilled offer");
    let base_isk = offer_unskilled.dict_get("iskReward").and_then(|v| v.as_i64()).unwrap();
    let base_lp = offer_unskilled.dict_get("lpReward").and_then(|v| v.as_i64()).unwrap();
    ctx.dispatch_rpc(&mut sess, "agentMgr", Some(sec_oid), "DoAction", &[PyRep::Integer(10)], &kw, None).await.expect("Defer unskilled offer");

    // Train Negotiation V (3356), Security Connections V (3895), Social V (3355)
    crate::common::slash_cmd(&ctx, &mut sess, "/giveskill 3356 5").await;
    crate::common::slash_cmd(&ctx, &mut sess, "/giveskill 3895 5").await;
    crate::common::slash_cmd(&ctx, &mut sess, "/giveskill 3355 5").await;

    let mut chosen_mid = 0_u32;
    let mut skilled_isk = 0_i64;
    let mut skilled_lp = 0_i64;
    for _ in 0..8 {
        let off = ctx.dispatch_rpc(&mut sess, "agentMgr", Some(sec_oid), "DoAction", &[PyRep::Integer(2)], &kw, None).await.expect("Skilled offer");
        let mid = off.dict_get("missionID").and_then(|v| v.as_i64()).unwrap() as u32;
        skilled_isk = off.dict_get("iskReward").and_then(|v| v.as_i64()).unwrap();
        skilled_lp = off.dict_get("lpReward").and_then(|v| v.as_i64()).unwrap();
        let npc_cnt = ctx.db.with_snapshot(|s| s.active_missions.get(&(char_id, mid)).map_or(0, |m| m.npcs_remaining));
        if npc_cnt > 0 {
            chosen_mid = mid;
            break;
        }
        ctx.dispatch_rpc(&mut sess, "agentMgr", Some(sec_oid), "DoAction", &[PyRep::Integer(10)], &kw, None).await.expect("Defer non-combat mission");
    }
    assert!(chosen_mid > 0, "Expected combat security mission");
    assert!(skilled_isk > base_isk, "Negotiation skill must increase ISK reward");
    assert!(skilled_lp > base_lp, "Connections skill must increase LP reward");

    ctx.dispatch_rpc(&mut sess, "agentMgr", Some(sec_oid), "DoAction", &[PyRep::Integer(3)], &kw, None).await.expect("Accept combat mission");
    ctx.advance_secs(31);
    ctx.dispatch_rpc(&mut sess, "stationSvc", None, "Undock", &[], &kw, None).await.expect("Undock for combat mission");

    let (expected_rem, mission_npc_ids) = ctx.db.with_snapshot(|s| {
        let m = &s.active_missions[&(char_id, chosen_mid)];
        (m.npcs_remaining, m.spawned_npc_ball_ids.clone())
    });
    assert!(!mission_npc_ids.is_empty(), "Mission must spawn its own NPCs");

    // Kill an ambient non-mission NPC in the same system -> must NOT decrement mission npcs_remaining
    let ambient_rep = crate::common::slash_cmd(&ctx, &mut sess, "/spawnnpc 10035").await;
    let ambient_id = ambient_rep.dict_get("npcID").or_else(|| ambient_rep.dict_get("ballID")).or_else(|| ambient_rep.dict_get("itemID")).and_then(|v| v.as_i64()).unwrap();
    let sys_h = ctx.systems.get_or_spawn(sec_sys);
    let lethal = eve_destiny::DamageVector { em: 100_000.0, thermal: 100_000.0, kinetic: 100_000.0, explosive: 100_000.0 };
    sys_h.apply_damage_with_attacker(ambient_id, lethal, Some(char_id)).await.unwrap().unwrap();
    let after_ambient = ctx.db.with_snapshot(|s| s.active_missions[&(char_id, chosen_mid)].npcs_remaining);
    assert_eq!(after_ambient, expected_rem, "Non-mission NPC kill must not advance mission objective");

    // Destroy mission's own spawned NPCs
    for npc_bid in mission_npc_ids {
        sys_h.apply_damage_with_attacker(npc_bid, lethal, Some(char_id)).await.unwrap().unwrap();
    }
    dock_pilot_at_station(&ctx, &mut sess, char_id, ship_id, sec_sys, sec_sta).await;

    let corp_id = sec_agent.corporation_id;
    let fac_id = ctx.sde.npc_corporations[&corp_id].faction_id.unwrap() as i64;
    let (ag_before, corp_before, fac_before) = ctx.db.with_snapshot(|s| {
        let st = &s.characters[&char_id].standings;
        (st.get(&sec_agent.agent_id).copied().unwrap_or(0.0), st.get(&corp_id).copied().unwrap_or(0.0), st.get(&fac_id).copied().unwrap_or(0.0))
    });
    let comp_sec = ctx.dispatch_rpc(&mut sess, "agentMgr", Some(sec_oid), "DoAction", &[PyRep::Integer(6)], &kw, None).await.expect("Complete security mission");
    assert_eq!(comp_sec.dict_get("state").and_then(|v| v.as_str()), Some("Completed"));
    let (ag_after, corp_after, fac_after) = ctx.db.with_snapshot(|s| {
        let st = &s.characters[&char_id].standings;
        (st.get(&sec_agent.agent_id).copied().unwrap_or(0.0), st.get(&corp_id).copied().unwrap_or(0.0), st.get(&fac_id).copied().unwrap_or(0.0))
    });
    assert!(ag_after > ag_before && corp_after > corp_before && fac_after > fac_before, "Standing must propagate to agent, corp, and faction");

    // Re-requesting from same agent does not re-offer completed mission
    let next_offer = ctx.dispatch_rpc(&mut sess, "agentMgr", Some(sec_oid), "DoAction", &[PyRep::Integer(2)], &kw, None).await.expect("Next offer");
    assert_ne!(next_offer.dict_get("missionID").and_then(|v| v.as_i64()), Some(chosen_mid as i64));
    let _ = ctx.dispatch_rpc(&mut sess, "agentMgr", Some(sec_oid), "DoAction", &[PyRep::Integer(10)], &kw, None).await;

    // 2b. Connections V (3359) lifts positive standing above 1.0 to unlock Level 2 agent, plus Defer/Decline/Accept/Quit
    let l2_same_fac = ctx.sde.agents.values()
        .find(|a| a.level == 2 && a.agent_type_id == 2 && a.station_id.is_some() && a.solar_system_id.is_some()
            && ctx.sde.npc_corporations.get(&a.corporation_id).and_then(|c| c.faction_id) == Some(fac_id as u32))
        .unwrap_or(&l2_agent).clone();
    dock_pilot_at_station(&ctx, &mut sess, char_id, ship_id, l2_same_fac.solar_system_id.unwrap(), l2_same_fac.station_id.unwrap()).await;
    let l2_fac_oid = bind_service_oid(&ctx, &mut sess, "agentMgr", PyRep::Integer(l2_same_fac.agent_id)).await;
    crate::common::slash_cmd(&ctx, &mut sess, "/giveskill 3359 5").await;
    let l2_offer = ctx.dispatch_rpc(&mut sess, "agentMgr", Some(l2_fac_oid), "DoAction", &[PyRep::Integer(2)], &kw, None).await.expect("L2 offer unlocked by Connections V");
    assert_eq!(l2_offer.dict_get("state").and_then(|v| v.as_str()), Some("Offered"));
    ctx.advance_secs(14_401);
    let def_rep = ctx.dispatch_rpc(&mut sess, "agentMgr", Some(l2_fac_oid), "DoAction", &[PyRep::Integer(10)], &kw, None).await.expect("DoAction(Defer)");
    assert_eq!(def_rep.dict_get("standingPenaltyApplied"), Some(&PyRep::Bool(false)));
    ctx.dispatch_rpc(&mut sess, "agentMgr", Some(l2_fac_oid), "DoAction", &[PyRep::Integer(2)], &kw, None).await.expect("Re-offer after defer");
    ctx.dispatch_rpc(&mut sess, "agentMgr", Some(l2_fac_oid), "DoAction", &[PyRep::Integer(9)], &kw, None).await.expect("DoAction(Decline)");
    ctx.dispatch_rpc(&mut sess, "agentMgr", Some(l2_fac_oid), "DoAction", &[PyRep::Integer(2)], &kw, None).await.expect("Re-offer before quit");
    ctx.dispatch_rpc(&mut sess, "agentMgr", Some(l2_fac_oid), "DoAction", &[PyRep::Integer(3)], &kw, None).await.expect("DoAction(Accept)");
    let quit_rep = ctx.dispatch_rpc(&mut sess, "agentMgr", Some(l2_fac_oid), "DoAction", &[PyRep::Integer(11)], &kw, None).await.expect("DoAction(Quit)");
    assert_eq!(quit_rep.dict_get("state").and_then(|v| v.as_str()), Some("Quit"));

    // 3. Courier mission requires delivery at destination station
    let dist_agent = ctx.sde.agents.values()
        .find(|a| a.level == 1 && a.division_id == 22 && a.agent_type_id == 2 && a.station_id.is_some() && a.solar_system_id.is_some())
        .expect("L1 Distribution agent")
        .clone();
    dock_pilot_at_station(&ctx, &mut sess, char_id, ship_id, dist_agent.solar_system_id.unwrap(), dist_agent.station_id.unwrap()).await;
    let dist_oid = bind_service_oid(&ctx, &mut sess, "agentMgr", PyRep::Integer(dist_agent.agent_id)).await;
    let mut courier_dest = 0_i64;
    for _ in 0..8 {
        let off = ctx.dispatch_rpc(&mut sess, "agentMgr", Some(dist_oid), "DoAction", &[PyRep::Integer(2)], &kw, None).await.expect("Courier offer");
        if let Some(dest) = off.dict_get("destinationStationID").and_then(|v| v.as_i64()) {
            if dest != dist_agent.station_id.unwrap() {
                courier_dest = dest;
                break;
            }
        }
        let _ = ctx.dispatch_rpc(&mut sess, "agentMgr", Some(dist_oid), "DoAction", &[PyRep::Integer(10)], &kw, None).await;
    }
    assert!(courier_dest > 0, "Expected courier mission with remote destinationStationID");
    ctx.dispatch_rpc(&mut sess, "agentMgr", Some(dist_oid), "DoAction", &[PyRep::Integer(3)], &kw, None).await.expect("Accept courier mission");
    let not_at_dest = ctx.dispatch_rpc(&mut sess, "agentMgr", Some(dist_oid), "DoAction", &[PyRep::Integer(6)], &kw, None).await.unwrap_err();
    assert_eq!(not_at_dest.code, "CourierNotAtDestinationStation");

    let courier_iid = ctx.db.with_snapshot(|s| {
        s.item_custom_names
            .iter()
            .find(|(iid, tag)| tag.starts_with(&format!("courier_cargo:{char_id}:")) && s.items.contains_key(iid))
            .map(|(iid, _)| *iid)
            .expect("Granted courier cargo item in station hangar")
    });
    ctx.dispatch_rpc(&mut sess, "invbroker", None, "Add", &[PyRep::Integer(courier_iid), PyRep::Integer(ship_id), PyRep::Integer(5)], &kw, None)
        .await
        .expect("Load courier cargo into active ship cargo hold");

    let dest_sys = ctx.sde.stations[&courier_dest].solar_system_id;
    dock_pilot_at_station(&ctx, &mut sess, char_id, ship_id, dest_sys, courier_dest).await;
    let courier_comp = ctx.dispatch_rpc(&mut sess, "agentMgr", Some(dist_oid), "DoAction", &[PyRep::Integer(6)], &kw, None).await.expect("Complete courier mission at destination");
    assert_eq!(courier_comp.dict_get("state").and_then(|v| v.as_str()), Some("Completed"));
}
