//! PVE Agent Finder, Conversation, Mission Accept/Decline/Quit Integration Tests (Part 2).

#![forbid(unsafe_code)]

use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use std::collections::HashMap;

async fn create_auth_session(ctx: &ServerContext, session_id: u64) -> SessionState {
    let kw = HashMap::new();
    let mut sess = SessionState::new(session_id);
    ctx.dispatch_rpc(
        &mut sess,
        "authentication",
        None,
        "Login",
        &[
            PyRep::String("demo".into()),
            PyRep::String("demo-secret".into()),
        ],
        &kw,
        None,
    )
    .await
    .expect("Login failed");
    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(90_000_001)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID failed");
    sess.solar_system_id = Some(30_000_142);
    sess.station_id = Some(60_003_760);
    sess.region_id = Some(10_000_002);
    sess
}

async fn bind_service_oid(
    ctx: &ServerContext,
    sess: &mut SessionState,
    service: &str,
    bind_param: PyRep,
) -> u64 {
    let kw = HashMap::new();
    let res = ctx
        .dispatch_rpc(
            sess,
            service,
            None,
            "MachoBindObject",
            &[bind_param, PyRep::None],
            &kw,
            None,
        )
        .await
        .expect("MachoBindObject failed");
    let PyRep::Tuple(outer) = res else {
        panic!("Expected Tuple from MachoBindObject");
    };
    outer[0].as_i64().expect("bound_id integer") as u64
}

// ============================================================================
// E-5: PVE Agent Finder, Conversation, Mission Accept/Decline/Quit
// ============================================================================
#[tokio::test]
async fn test_e5_agent_finder_and_conversation_lifecycle() {
    // parity: plan:E-5, feat:pve.agent-finder, feat:pve.agent-conversation, feat:pve.mission-accept, feat:pve.mission-decline, feat:pve.mission-quit, rpc:agentMgr.GetAgents, rpc:agentMgr.GetMyJournalDetails, rpc:agentMgr.GetSolarSystemOfAgent
    eve_testkit::attest(&["plan:E-5", "feat:pve.agent-finder", "feat:pve.agent-conversation", "feat:pve.mission-accept", "feat:pve.mission-decline", "feat:pve.mission-quit"]);
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let mut sess = create_auth_session(&ctx, 1002).await;
    let kw = HashMap::new();
    let char_id = 90_000_001_i64;

    // 1. Agent Finder (feat:pve.agent-finder)
    let agents_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "agentMgr",
            None,
            "GetAgents",
            &[PyRep::Integer(1), PyRep::Integer(24)],
            &kw,
            None,
        )
        .await
        .expect("agentMgr.GetAgents");

    let PyRep::Rowset {
        columns: agents_cols,
        rows: agents_rows,
    } = agents_rep
    else {
        panic!("Expected Rowset from GetAgents");
    };
    assert_eq!(
        agents_cols,
        vec![
            "agentID",
            "agentTypeID",
            "divisionID",
            "corporationID",
            "factionID",
            "solarsystemID",
            "stationID",
            "level",
            "quality",
            "hasAccess",
        ]
    );
    assert!(!agents_rows.is_empty(), "Level 1 Security agents must be found");

    for row in &agents_rows {
        assert_eq!(row[7].as_i64(), Some(1), "Level must be 1");
        assert_eq!(row[2].as_i64(), Some(24), "Division must be 24");
        assert_eq!(row[9], PyRep::Bool(true), "Level 1 agents must have hasAccess = true");
    }

    let l2_agents_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "agentMgr",
            None,
            "GetAgents",
            &[PyRep::Integer(2), PyRep::Integer(24)],
            &kw,
            None,
        )
        .await
        .expect("agentMgr.GetAgents L2");

    let PyRep::Rowset { rows: l2_rows, .. } = l2_agents_rep else {
        panic!("Expected Rowset");
    };
    assert!(!l2_rows.is_empty(), "Level 2 Security agents must be found");
    let test_l2_agent_id = l2_rows[0][0].as_i64().unwrap();
    let test_l2_corp_id = l2_rows[0][3].as_i64().unwrap();
    assert_eq!(l2_rows[0][9], PyRep::Bool(false), "L2 agent requires standing >= 1.0");

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.standings.insert(test_l2_corp_id, 3.0);
        }
    });

    let l2_accessible_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "agentMgr",
            None,
            "GetAgents",
            &[PyRep::Integer(2), PyRep::Integer(24)],
            &kw,
            None,
        )
        .await
        .expect("agentMgr.GetAgents L2 with standing");

    let PyRep::Rowset { rows: l2_acc_rows, .. } = l2_accessible_rep else {
        panic!("Expected Rowset");
    };
    let accessible_row = l2_acc_rows
        .iter()
        .find(|r| r[0].as_i64() == Some(test_l2_agent_id))
        .unwrap();
    assert_eq!(accessible_row[9], PyRep::Bool(true));

    // 2. Agent Conversation via V24 bound agentMgr moniker (feat:pve.agent-conversation)
    let l1_agent = agents_rows
        .iter()
        .find(|r| r[5].as_i64() == Some(30_000_142))
        .unwrap_or(&agents_rows[0]);
    let l1_agent_id = l1_agent[0].as_i64().unwrap();
    let l1_agent_sys = l1_agent[5].as_i64().unwrap() as u32;
    let l1_agent_sta = l1_agent[6].as_i64();
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = l1_agent_sys;
            ch.station_id = l1_agent_sta;
        }
    });
    sess.solar_system_id = Some(l1_agent_sys);
    sess.station_id = l1_agent_sta;

    let agent_oid = bind_service_oid(&ctx, &mut sess, "agentMgr", PyRep::Integer(l1_agent_id)).await;

    let info_res = ctx
        .dispatch_rpc(&mut sess, "agentMgr", Some(agent_oid), "GetInfoServiceDetails", &[], &kw, None)
        .await
        .expect("agentMgr.GetInfoServiceDetails");
    assert_eq!(info_res.dict_get("agentID").and_then(|v| v.as_i64()), Some(l1_agent_id));

    let agent_sys_res = ctx
        .dispatch_rpc(&mut sess, "agentMgr", Some(agent_oid), "GetSolarSystemOfAgent", &[], &kw, None)
        .await
        .expect("agentMgr.GetSolarSystemOfAgent");
    assert_eq!(agent_sys_res.as_i64(), Some(l1_agent_sys as i64));

    let conv_res = ctx
        .dispatch_rpc(&mut sess, "agentMgr", Some(agent_oid), "DoAction", &[PyRep::Integer(2)], &kw, None)
        .await
        .expect("agentMgr.DoAction(RequestMission)");

    let PyRep::KeyVal(conv_kv) = conv_res else {
        panic!("Expected KeyVal from DoAction(RequestMission)");
    };
    let mission_id = conv_kv["missionID"].as_i64().expect("missionID") as u32;
    assert_eq!(conv_kv["agentID"].as_i64(), Some(l1_agent_id));
    assert_eq!(conv_kv["state"].as_str(), Some("Offered"));
    assert_eq!(conv_kv["missionState"].as_i64(), Some(1));
    assert!(conv_kv["briefing"].as_str().is_some());
    assert!(conv_kv["objectives"].as_str().is_some());
    assert!(conv_kv["iskReward"].as_i64().unwrap() > 0);
    assert!(conv_kv["lpReward"].as_i64().unwrap() > 0);
    assert!(conv_kv["expiryUnix"].as_i64().unwrap() > 0);

    // 3. Mission Accept via DoAction(3) (feat:pve.mission-accept)
    let accept_res = ctx
        .dispatch_rpc(&mut sess, "agentMgr", Some(agent_oid), "DoAction", &[PyRep::Integer(3)], &kw, None)
        .await
        .expect("agentMgr.DoAction(Accept)");

    let PyRep::KeyVal(acc_kv) = accept_res else {
        panic!("Expected KeyVal from DoAction(Accept)");
    };
    assert_eq!(acc_kv["missionID"].as_i64(), Some(mission_id as i64));
    assert_eq!(acc_kv["state"].as_str(), Some("Accepted"));
    assert_eq!(acc_kv["missionState"].as_i64(), Some(2));
    assert!(acc_kv["bookmarkID"].as_i64().is_some());
    assert!(acc_kv["expiryUnix"].as_i64().is_some());
    assert!(acc_kv["bonusExpiryUnix"].as_i64().is_some());

    let bm_id = acc_kv["bookmarkID"].as_i64().unwrap();
    assert!(ctx.db.with_snapshot(|snap| snap.bookmarks.contains_key(&bm_id)));

    let journal_res = ctx
        .dispatch_rpc(&mut sess, "agentMgr", None, "GetMyJournalDetails", &[], &kw, None)
        .await
        .expect("agentMgr.GetMyJournalDetails");
    let PyRep::Tuple(j_tuple) = journal_res else {
        panic!("Expected Tuple from GetMyJournalDetails");
    };
    let PyRep::List(j_missions) = &j_tuple[0] else {
        panic!("Expected missions List");
    };
    assert!(
        j_missions.iter().any(|m| m.dict_get("missionID").and_then(|v| v.as_i64()) == Some(mission_id as i64)
            && m.dict_get("missionState").and_then(|v| v.as_i64()) == Some(2))
    );

    let briefing_info = ctx
        .dispatch_rpc(&mut sess, "agentMgr", Some(agent_oid), "GetMissionBriefingInfo", &[], &kw, None)
        .await
        .expect("agentMgr.GetMissionBriefingInfo");
    assert_eq!(briefing_info.dict_get("missionID").and_then(|v| v.as_i64()), Some(mission_id as i64));

    // 4. Mission Quit via DoAction(11) (feat:pve.mission-quit)
    let (initial_agent_st, initial_corp_st) = ctx.db.with_snapshot(|snap| {
        let ch = &snap.characters[&char_id];
        (
            ch.standings.get(&l1_agent_id).copied().unwrap_or(0.0),
            ch.standings.get(&l1_agent[3].as_i64().unwrap()).copied().unwrap_or(0.0),
        )
    });

    let quit_res = ctx
        .dispatch_rpc(&mut sess, "agentMgr", Some(agent_oid), "DoAction", &[PyRep::Integer(11)], &kw, None)
        .await
        .expect("agentMgr.DoAction(Quit)");

    let PyRep::KeyVal(quit_kv) = quit_res else {
        panic!("Expected KeyVal from DoAction(Quit)");
    };
    assert_eq!(quit_kv["state"].as_str(), Some("Quit"));
    assert_eq!(quit_kv["missionState"].as_i64(), Some(6));
    assert_eq!(quit_kv["dungeonCleaned"], PyRep::Bool(true));
    assert_eq!(quit_kv["standingPenaltyApplied"], PyRep::Bool(true));

    let (post_quit_agent_st, post_quit_corp_st) = ctx.db.with_snapshot(|snap| {
        let ch = &snap.characters[&char_id];
        (
            ch.standings.get(&l1_agent_id).copied().unwrap_or(0.0),
            ch.standings.get(&l1_agent[3].as_i64().unwrap()).copied().unwrap_or(0.0),
        )
    });
    assert!((post_quit_agent_st - (initial_agent_st - 0.1)).abs() < 1e-4);
    assert!((post_quit_corp_st - (initial_corp_st - 0.1)).abs() < 1e-4);
    assert!(!ctx.db.with_snapshot(|snap| snap.bookmarks.contains_key(&bm_id)));

    let journal_after_quit = ctx
        .dispatch_rpc(&mut sess, "agentMgr", None, "GetMyJournalDetails", &[], &kw, None)
        .await
        .expect("GetMyJournalDetails after quit");
    let PyRep::Tuple(jq_sections) = journal_after_quit else { panic!() };
    let PyRep::List(jq_missions) = &jq_sections[0] else { panic!() };
    assert!(jq_missions.is_empty());

    // 5. Mission Decline via DoAction(9) (feat:pve.mission-decline)
    let offer2_res = ctx
        .dispatch_rpc(&mut sess, "agentMgr", Some(agent_oid), "DoAction", &[PyRep::Integer(2)], &kw, None)
        .await
        .expect("agentMgr.DoAction(RequestMission) 2");
    let PyRep::KeyVal(offer2_kv) = offer2_res else { panic!() };
    let _mid2 = offer2_kv["missionID"].as_i64().unwrap() as u32;

    let decline1_res = ctx
        .dispatch_rpc(&mut sess, "agentMgr", Some(agent_oid), "DoAction", &[PyRep::Integer(9)], &kw, None)
        .await
        .expect("agentMgr.DoAction(Decline) 1");
    let PyRep::KeyVal(dec1_kv) = decline1_res else { panic!() };
    assert_eq!(dec1_kv["state"].as_str(), Some("Declined"));
    assert_eq!(dec1_kv["missionState"].as_i64(), Some(5));
    assert_eq!(dec1_kv["standingPenaltyApplied"], PyRep::Bool(false));
    let st_after_dec1 = ctx.db.with_snapshot(|snap| {
        snap.characters[&char_id].standings.get(&l1_agent[3].as_i64().unwrap()).copied().unwrap_or(0.0)
    });
    assert!((st_after_dec1 - post_quit_corp_st).abs() < 1e-6);

    let offer3_res = ctx
        .dispatch_rpc(&mut sess, "agentMgr", Some(agent_oid), "DoAction", &[PyRep::Integer(2)], &kw, None)
        .await
        .expect("agentMgr.DoAction(RequestMission) 3");
    let PyRep::KeyVal(offer3_kv) = offer3_res else { panic!() };
    let _mid3 = offer3_kv["missionID"].as_i64().unwrap() as u32;

    let decline2_res = ctx
        .dispatch_rpc(&mut sess, "agentMgr", Some(agent_oid), "DoAction", &[PyRep::Integer(9)], &kw, None)
        .await
        .expect("agentMgr.DoAction(Decline) 2");
    let PyRep::KeyVal(dec2_kv) = decline2_res else { panic!() };
    assert_eq!(dec2_kv["state"].as_str(), Some("Declined"));
    assert_eq!(dec2_kv["standingPenaltyApplied"], PyRep::Bool(true));
    let st_after_dec2 = ctx.db.with_snapshot(|snap| {
        snap.characters[&char_id].standings.get(&l1_agent[3].as_i64().unwrap()).copied().unwrap_or(0.0)
    });
    assert!(st_after_dec2 < st_after_dec1);
}
