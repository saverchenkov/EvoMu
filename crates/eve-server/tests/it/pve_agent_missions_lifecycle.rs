//! Integration tests for PVE Agent Finder, Conversation, and Full Mission Lifecycle (`E-5`).
//!
//! Covers parity rows:
//! - `feat:pve.agent-finder`: Agent finder queries filtered by corporation, faction, level, security status, and agent type.
//! - `feat:pve.agent-conversation`: Agent interaction dialog, greeting, mission briefing, offer details (`agentMgr.GetOffer` / `DoAction`).
//! - `feat:pve.mission-accept`: Mission acceptance lifecycle state transition from Offered to Accepted.
//! - `feat:pve.mission-decline`: Mission decline mechanics, cooldowns, and standings impact.
//! - `feat:pve.mission-quit`: Mission abort / quit mechanics and failure state.
//! - `feat:pve.mission-complete`: Mission objective completion, ISK/LP reward payout, bonus time rewards, and turn-in.
//! - `feat:pve.mission-lp-isk-reward-formula`: Base LP * (1.6288 - true_sec) * division Connections LP and +5%/lvl Negotiation ISK/bonus scaling.
//! - `plan:E-5`: Canonical agent mission offer -> briefing/objective -> accept -> completion -> reward/standing payout lifecycle.
//! - `feat:pve.mission-expire`: Offered mission expiration timeout and cleanup.
//! - `feat:pve.mission-journal`: Character active and completed mission journal entries and bookmarks.
//! - `feat:pve.mission-tracker`: HUD mission tracker objective tracking, dungeon room progression, and NPC kill counts.

#![forbid(unsafe_code)]

use crate::common::{new_wire_harness, ServerWireTransport};
use crate::scenario_common::ServerScenarioBackend;
use eve_db::{BookmarkRecord, InventoryFlag, IskCents};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_testkit::client_harness::ClientHarness;
use eve_testkit::legal_fixture::{assert_legal_fixture, FixtureSessionView, LegalFixtureExpectation};
use eve_testkit::scenario::ScenarioBackend as _;
use std::collections::HashMap;

async fn assert_pilot_fixture_legal(
    label: &str,
    ctx: &ServerContext,
    sess: &SessionState,
    expected_dock_location_id: Option<i64>,
    expect_in_space: bool,
) {
    let ship_in_ballpark = if expect_in_space {
        match (sess.ship_id, sess.solar_system_id) {
            (Some(ship_id), Some(sys_id)) => match ctx.systems.get_handle(sys_id) {
                Some(h) => h.get_ball_snapshot(ship_id).await.ok().flatten().is_some(),
                None => false,
            },
            _ => false,
        }
    } else {
        false
    };
    let view = FixtureSessionView {
        session_id: sess.session_id,
        user_id: sess.user_id,
        character_id: sess.character_id,
        corporation_id: sess.corporation_id,
        alliance_id: sess.alliance_id,
        station_id: sess.station_id,
        structure_id: sess.structure_id,
        solar_system_id: sess.solar_system_id,
        ship_id: sess.ship_id,
        corp_role: sess.corp_role,
        fleet_id: sess.fleet_id,
        role: sess.role,
        now_unix: Some(ctx.now_unix()),
    };
    let snap = ctx.db.with_snapshot(Clone::clone);
    assert_legal_fixture(
        label,
        &snap,
        &ctx.sde,
        &view,
        &LegalFixtureExpectation {
            expected_dock_location_id,
            expect_in_space: Some(expect_in_space),
            ship_in_space_ballpark: expect_in_space.then_some(ship_in_ballpark),
            ..Default::default()
        },
    );
}

async fn relocate_docked_pilot(
    ctx: &ServerContext,
    sess: &mut SessionState,
    char_id: i64,
    sys_id: u32,
    station_id: Option<i64>,
) {
    let sys_meta = ctx
        .sde
        .solar_systems
        .get(&sys_id)
        .unwrap_or_else(|| panic!("unknown solar system {sys_id} in SDE"));
    let constellation_id = sys_meta.constellation_id;
    let region_id = sys_meta.region_id;

    let (ship_id, prev_sys, spawn_type_id) = ctx.db.with_mut_snapshot(|snap| {
        let mut ship_id = 0_i64;
        let mut prev_sys = sys_id;
        let mut spawn_type_id = None;
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            prev_sys = ch.solar_system_id;
            ch.solar_system_id = sys_id;
            ch.constellation_id = constellation_id;
            ch.region_id = region_id;
            ch.station_id = station_id;
            ch.structure_id = None;
            ship_id = ch.active_ship_id;
        }
        if ship_id > 0 {
            if let Some(ship) = snap.items.get_mut(&ship_id) {
                match station_id {
                    Some(sta) => {
                        ship.location_id = sta;
                        ship.flag = InventoryFlag::Hangar;
                        snap.item_positions.remove(&ship_id);
                    }
                    None => {
                        ship.location_id = i64::from(sys_id);
                        ship.flag = InventoryFlag::None;
                        snap.item_positions.entry(ship_id).or_insert([0.0, 0.0, 0.0]);
                        spawn_type_id = Some(ship.type_id);
                    }
                }
            }
        }
        (ship_id, prev_sys, spawn_type_id)
    });

    if ship_id > 0 && prev_sys != sys_id {
        if let Some(prev_h) = ctx.systems.get_handle(prev_sys) {
            let _ = prev_h.remove_ball(ship_id).await;
        }
    }
    if let (Some(type_id), None) = (spawn_type_id, station_id) {
        let sys_h = ctx.systems.get_or_spawn(sys_id);
        if sys_h.get_ball_snapshot(ship_id).await.ok().flatten().is_none() {
            let _ = sys_h
                .spawn_ball_with_meta(
                    eve_destiny::Ball {
                        id: ship_id,
                        type_id,
                        position: glam::DVec3::ZERO,
                        velocity: glam::DVec3::ZERO,
                        max_velocity: 365.0,
                        inertia: 1.0,
                        mass: 1_200_000.0,
                        radius: 40.0,
                        speed_fraction: 0.0,
                        mode: eve_destiny::MovementMode::Stop,
                    },
                    ctx.build_ship_combat_state(char_id, ship_id, type_id),
                    ctx.build_ship_slim_meta(char_id, type_id),
                )
                .await;
        }
    }

    sess.solar_system_id = Some(sys_id);
    sess.constellation_id = Some(constellation_id);
    sess.region_id = Some(region_id);
    sess.station_id = station_id;
    sess.structure_id = None;
    if ship_id > 0 {
        sess.ship_id = Some(ship_id);
    }

    assert_pilot_fixture_legal(
        "relocate_docked_pilot",
        ctx,
        sess,
        station_id,
        station_id.is_none(),
    )
    .await;
}

async fn auth_pilot_harness<'c>(
    ctx: &'c ServerContext,
    session_id: u64,
    user: &str,
    pass: &str,
    char_id: i64,
    grant_starter_ship_skills: bool,
) -> ClientHarness<ServerWireTransport<'c>> {
    let kw = HashMap::new();
    let mut harness = new_wire_harness(ctx, session_id);
    let login_rep = harness
        .call_ok(
            "authentication",
            None,
            "Login",
            &[PyRep::String(user.into()), PyRep::String(pass.into())],
            &kw,
        )
        .await;
    assert!(
        matches!(login_rep, PyRep::KeyVal(_) | PyRep::Dict(_)),
        "Login failed: {login_rep:?}"
    );
    let sel_rep = harness
        .call_ok(
            "charUnboundMgr",
            None,
            "SelectCharacterID",
            &[PyRep::Integer(char_id)],
            &kw,
        )
        .await;
    assert!(
        matches!(sel_rep, PyRep::KeyVal(_) | PyRep::Dict(_)),
        "SelectCharacterID failed: {sel_rep:?}"
    );
    if grant_starter_ship_skills {
        for skill_id in [3327_i64, 3329_i64] {
            let sk_rep = harness
                .call_ok(
                    "slash",
                    None,
                    "SlashCmd",
                    &[PyRep::String(format!("/giveskill {skill_id} 5"))],
                    &kw,
                )
                .await;
            assert_eq!(sk_rep.dict_get("level"), Some(&PyRep::Integer(5)));
        }
    }
    let backend = ServerScenarioBackend::new(ctx);
    let view = backend.actor_session_view(harness.transport());
    let snap = ctx.db.with_snapshot(|s| s.clone());
    assert_legal_fixture(
        "pve_agent_missions_lifecycle_pilot",
        &snap,
        &ctx.sde,
        &view,
        &LegalFixtureExpectation {
            expect_in_space: Some(false),
            ..Default::default()
        },
    );
    harness
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
    let mut sess = auth_pilot_harness(&ctx, 2001, "demo", "demo-secret", char_id, false)
        .await
        .into_transport()
        .inner
        .into_session();
    let kw = HashMap::new();

    // 1. Agent Finder queries (feat:pve.agent-finder)
    // 1a. Query Level 1 Security (division 24) agents
    let l1_rep = ctx.dispatch_rpc(&mut sess, "agentMgr", None, "GetAgents", &[PyRep::Integer(1), PyRep::Integer(24)], &kw, None).await.expect("GetAgents L1");
    let PyRep::Rowset { columns: agents_cols, rows: l1_rows } = l1_rep else { panic!("Expected Rowset from GetAgents") };
    assert_eq!(agents_cols, vec!["agentID", "agentTypeID", "divisionID", "corporationID", "factionID", "solarsystemID", "stationID", "level", "quality", "hasAccess", "isLocatorAgent"]);
    assert!(!l1_rows.is_empty(), "Level 1 Security agents must be found");
    for row in &l1_rows {
        assert_eq!(row[7].as_i64(), Some(1), "Agent level must be 1");
        assert_eq!(row[2].as_i64(), Some(24), "Division must be 24");
        assert_eq!(row[9], PyRep::Bool(true), "L1 agents must have access by default");
        assert!(matches!(row[10], PyRep::Bool(_)), "isLocatorAgent must be bool (#636)");
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

    relocate_docked_pilot(&ctx, &mut sess, char_id, target_sys, target_station).await;

    let agent_oid = bind_service_oid(&ctx, &mut sess, "agentMgr", PyRep::Integer(target_agent_id)).await;

    // 2a. Query agent details and system
    let details = ctx.dispatch_rpc(&mut sess, "agentMgr", Some(agent_oid), "GetInfoServiceDetails", &[], &kw, None).await.expect("GetInfoServiceDetails");
    assert_eq!(details.dict_get("agentID").and_then(|v| v.as_i64()), Some(target_agent_id));
    assert_eq!(details.dict_get("level").and_then(|v| v.as_i64()), Some(1));
    if let Some(PyRep::List(services)) = details.dict_get("services") {
        assert!(services.iter().any(|s| s.as_str() == Some("mission") || s.dict_get("agentServiceType").and_then(|v| v.as_str()) == Some("mission")));
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
    let raw_briefing = briefing_info.dict_get("briefing").and_then(|v| v.as_str()).unwrap_or("");
    assert!(!raw_briefing.is_empty());
    assert!(!raw_briefing.contains("Execute objectives in system"), "GetMissionBriefingInfo must not emit fixed template: {raw_briefing}");
    if let Some(title) = ctx.sde.missions.get(&(mission_id as u32)).map(|m| m.title.as_str()).filter(|s| !s.is_empty()) {
        assert!(raw_briefing.contains(title), "GetMissionBriefingInfo must include mission title {title:?}: {raw_briefing}");
    }
}

#[tokio::test]
async fn test_pve_mission_lifecycle_accept_tracker_complete_journal() {
    // parity: feat:pve.mission-accept, feat:pve.mission-journal, feat:pve.mission-tracker
    eve_testkit::attest(&["feat:pve.mission-accept", "feat:pve.mission-journal", "feat:pve.mission-tracker"]);
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let char_id = 90_000_001_i64;
    let mut sess = auth_pilot_harness(&ctx, 2002, "demo", "demo-secret", char_id, false)
        .await
        .into_transport()
        .inner
        .into_session();
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
            ch.balance = IskCents::from_isk(10_000_000);
            ch.standings.insert(corp_id, 0.0);
            ch.loyalty_points.insert(corp_id, 100);
        }
    });
    relocate_docked_pilot(&ctx, &mut sess, char_id, sys_id, station_id).await;

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
    let PyRep::List(journal_missions) = &j_sections[0] else { panic!() };
    let PyRep::Tuple(j_entry) = journal_missions
        .iter()
        .find(|m| matches!(m, PyRep::Tuple(t) if t.get(9).and_then(|v| v.as_i64()) == Some(mission_id as i64)))
        .expect("Mission must be present in journal")
    else {
        panic!("Expected V24 10-tuple in GetMyJournalDetails");
    };
    assert_eq!(j_entry.len(), 10);
    assert_eq!(j_entry[0].as_i64(), Some(2));
    if let PyRep::List(bms) = &j_entry[6] {
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
        eve_server::services::mission_fixture::edit(snap, char_id, mission_id, |m| {
            m.npcs_remaining = 0;
        });
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
    assert_eq!(post_lp, initial_lp + paid_lp, "NPC corps charge 0% LP tax");

    let journal_entries = ctx.db.with_snapshot(|snap| {
        snap.wallet_journal.iter().filter(|e| e.character_id == char_id).map(|e| e.entry_type.clone()).collect::<Vec<_>>()
    });
    assert!(journal_entries.contains(&"agent_mission_reward".to_string()));
    assert!(journal_entries.contains(&"agent_mission_time_bonus_reward".to_string()));

    let post_journal = ctx.dispatch_rpc(&mut sess, "agentMgr", None, "GetMyJournalDetails", &[], &kw, None).await.expect("GetMyJournalDetails after completion");
    let PyRep::Tuple(post_tuple) = post_journal else { panic!() };
    let PyRep::List(post_missions) = &post_tuple[0] else { panic!() };
    assert!(!post_missions.iter().any(|m| matches!(m, PyRep::Tuple(t) if t.get(9).and_then(|v| v.as_i64()) == Some(mission_id as i64))));
}

#[tokio::test]
async fn test_pve_mission_decline_quit_and_expiration() {
    // parity: feat:pve.mission-decline, feat:pve.mission-quit, feat:pve.mission-expire
    eve_testkit::attest(&["feat:pve.mission-decline", "feat:pve.mission-quit", "feat:pve.mission-expire"]);
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let char_id = 90_000_001_i64;
    let mut sess = auth_pilot_harness(&ctx, 2003, "demo", "demo-secret", char_id, false)
        .await
        .into_transport()
        .inner
        .into_session();
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
            ch.standings.insert(corp_id, 2.0);
            ch.standings.insert(agent_id, 2.0);
        }
    });
    relocate_docked_pilot(&ctx, &mut sess, char_id, sys_id, station_id).await;

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
        eve_server::services::mission_fixture::edit(snap, char_id, mid_4, |m| {
            m.offer_expiry_unix = 500_u64;
        });
    });

    let accept_expired = ctx.dispatch_rpc(&mut sess, "agentMgr", Some(agent_oid), "DoAction", &[PyRep::Integer(3)], &kw, None).await.expect_err("Expired offer must reject accept");
    assert_eq!(accept_expired.code, "MissionOfferExpired");

    let j_exp = ctx.dispatch_rpc(&mut sess, "agentMgr", None, "GetMyJournalDetails", &[], &kw, None).await.expect("Journal sweep");
    let PyRep::Tuple(je_tup) = j_exp else { panic!() };
    let PyRep::List(je_list) = &je_tup[0] else { panic!() };
    assert!(!je_list.iter().any(|m| matches!(m, PyRep::Tuple(t) if t.get(9).and_then(|v| v.as_i64()) == Some(mid_4 as i64))));
    assert_eq!(ctx.db.with_snapshot(|snap| eve_server::services::mission_fixture::get(snap, char_id, mid_4).unwrap().state), "Expired");

    // 3b. Accepted mission deadline expiration
    let offer_5 = ctx.dispatch_rpc(&mut sess, "agentMgr", Some(agent_oid), "DoAction", &[PyRep::Integer(2)], &kw, None).await.expect("Offer 5");
    let PyRep::KeyVal(o5_kv) = offer_5 else { panic!() };
    let mid_5 = o5_kv["missionID"].as_i64().unwrap() as u32;

    ctx.dispatch_rpc(&mut sess, "agentMgr", Some(agent_oid), "DoAction", &[PyRep::Integer(3)], &kw, None).await.expect("Accept 5");

    ctx.db.with_mut_snapshot(|snap| {
        eve_server::services::mission_fixture::edit(snap, char_id, mid_5, |m| {
            m.accept_expiry_unix = Some(500_u64);
        });
    });

    let comp_expired = ctx.dispatch_rpc(&mut sess, "agentMgr", Some(agent_oid), "DoAction", &[PyRep::Integer(6)], &kw, None).await.expect_err("Completing expired mission must fail");
    assert_eq!(comp_expired.code, "MissionExpired");
    assert_eq!(ctx.db.with_snapshot(|snap| eve_server::services::mission_fixture::get(snap, char_id, mid_5).unwrap().state), "Failed");
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
    assert_pilot_fixture_legal("dock_pilot_at_station", ctx, sess, Some(station_id), false).await;
}

#[tokio::test]
async fn test_live_plan_e5_agent_missions_end_to_end() {
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let char_id = 96_005_001_i64;
    ctx.db.provision_gm_account("e5pilot", "e5-secret", char_id, "E5 Mission Pilot");
    let kw = HashMap::new();
    let mut sess = auth_pilot_harness(&ctx, 9605, "e5pilot", "e5-secret", char_id, true)
        .await
        .into_transport()
        .inner
        .into_session();

    let ship_id = ctx.db.with_snapshot(|s| s.characters[&char_id].active_ship_id);

    let is_empire_agent = |a: &&eve_sde::AgentInfo| {
        ctx.sde
            .npc_corporations
            .get(&a.corporation_id)
            .and_then(|c| c.faction_id)
            .is_some_and(|fid| matches!(fid, 500_001..=500_004))
    };

    // 1. Level 2 agent rejects character at 0.0 standing
    let l2_agent = ctx
        .sde
        .agents
        .values()
        .filter(|a| {
            a.level == 2
                && a.division_id == 24
                && a.agent_type_id == 2
                && a.station_id.is_some()
                && a.solar_system_id.is_some()
                && is_empire_agent(a)
        })
        .min_by_key(|a| a.agent_id)
        .expect("L2 Security agent")
        .clone();
    dock_pilot_at_station(&ctx, &mut sess, char_id, ship_id, l2_agent.solar_system_id.unwrap(), l2_agent.station_id.unwrap()).await;
    let l2_oid = bind_service_oid(&ctx, &mut sess, "agentMgr", PyRep::Integer(l2_agent.agent_id)).await;
    let gate_err = ctx.dispatch_rpc(&mut sess, "agentMgr", Some(l2_oid), "DoAction", &[PyRep::Integer(2)], &kw, None).await.unwrap_err();
    assert_eq!(gate_err.code, "InsufficientAgentStanding");

    // 2. Security encounter mission: skill scaling, ambient NPC non-interference, spawned NPC kills, and standing propagation
    let sec_agent = ctx
        .sde
        .agents
        .values()
        .filter(|a| {
            a.level == 1
                && a.division_id == 24
                && a.agent_type_id == 2
                && a.station_id.is_some()
                && a.solar_system_id.is_some()
                && is_empire_agent(a)
        })
        .min_by_key(|a| a.agent_id)
        .expect("L1 Security agent")
        .clone();
    let sec_sys = sec_agent.solar_system_id.unwrap();
    let sec_sta = sec_agent.station_id.unwrap();
    dock_pilot_at_station(&ctx, &mut sess, char_id, ship_id, sec_sys, sec_sta).await;
    let sec_oid = bind_service_oid(&ctx, &mut sess, "agentMgr", PyRep::Integer(sec_agent.agent_id)).await;

    // Roll a combat security mission first while unskilled, then verify Negotiation V & Security Connections V on that same mission
    let mut chosen_mid = 0_u32;
    let mut base_isk = 0_i64;
    let mut base_lp = 0_i64;
    let mut base_bonus = 0_i64;
    for _ in 0..8 {
        let off = ctx.dispatch_rpc(&mut sess, "agentMgr", Some(sec_oid), "DoAction", &[PyRep::Integer(2)], &kw, None).await.expect("Unskilled offer");
        let mid = off.dict_get("missionID").and_then(|v| v.as_i64()).unwrap() as u32;
        base_isk = off.dict_get("iskReward").and_then(|v| v.as_i64()).unwrap();
        base_lp = off.dict_get("lpReward").and_then(|v| v.as_i64()).unwrap();
        base_bonus = off.dict_get("bonusReward").and_then(|v| v.as_i64()).unwrap();
        let npc_cnt = ctx.db.with_snapshot(|s| eve_server::services::mission_fixture::get(s, char_id, mid).map_or(0, |m| m.npcs_remaining));
        if npc_cnt > 0 {
            chosen_mid = mid;
            break;
        }
        ctx.dispatch_rpc(&mut sess, "agentMgr", Some(sec_oid), "DoAction", &[PyRep::Integer(10)], &kw, None).await.expect("Defer non-combat mission");
    }
    assert!(chosen_mid > 0, "Expected combat security mission");

    // Train Negotiation V (3356), Security Connections V (3895), Social V (3355)
    crate::common::slash_cmd(&ctx, &mut sess, "/giveskill 3356 5").await;
    crate::common::slash_cmd(&ctx, &mut sess, "/giveskill 3895 5").await;
    crate::common::slash_cmd(&ctx, &mut sess, "/giveskill 3355 5").await;

    let obj_skilled = ctx.dispatch_rpc(&mut sess, "agentMgr", Some(sec_oid), "GetMissionObjectiveInfo", &[], &kw, None).await.expect("GetMissionObjectiveInfo skilled");
    let Some(PyRep::List(norm_rw)) = obj_skilled.dict_get("normalRewards") else { panic!("expected normalRewards") };
    let PyRep::Tuple(norm_tup) = &norm_rw[0] else { panic!("expected normalRewards tuple") };
    let skilled_isk = norm_tup[1].as_i64().unwrap();
    let skilled_lp = obj_skilled.dict_get("loyaltyPoints").and_then(|v| v.as_i64()).unwrap();
    let Some(PyRep::List(bon_rw)) = obj_skilled.dict_get("bonusRewards") else { panic!("expected bonusRewards") };
    let PyRep::Tuple(bon_tup) = &bon_rw[0] else { panic!("expected bonusRewards tuple") };
    let skilled_bonus = bon_tup[2].as_i64().unwrap();
    assert_eq!(skilled_isk, ((base_isk as f64) * 1.25).round() as i64, "Negotiation V must give +25% ISK");
    assert_eq!(skilled_bonus, ((base_bonus as f64) * 1.25).round() as i64, "Negotiation V must give +25% bonus ISK");
    assert!(skilled_lp > base_lp, "Connections V must increase LP reward");

    ctx.dispatch_rpc(&mut sess, "agentMgr", Some(sec_oid), "DoAction", &[PyRep::Integer(3)], &kw, None).await.expect("Accept combat mission");
    ctx.advance_secs(31);
    ctx.dispatch_rpc(&mut sess, "stationSvc", None, "Undock", &[], &kw, None).await.expect("Undock for combat mission");

    let (expected_rem, mission_npc_ids) = ctx.db.with_snapshot(|s| {
        let m = eve_server::services::mission_fixture::get(s, char_id, chosen_mid).unwrap();
        (m.npcs_remaining, m.spawned_npc_ball_ids)
    });
    assert!(!mission_npc_ids.is_empty(), "Mission must spawn its own NPCs");

    // Kill an ambient non-mission NPC in the same system -> must NOT decrement mission npcs_remaining
    let ambient_rep = crate::common::slash_cmd(&ctx, &mut sess, "/spawnnpc 10035").await;
    let ambient_id = ambient_rep.dict_get("npcID").or_else(|| ambient_rep.dict_get("ballID")).or_else(|| ambient_rep.dict_get("itemID")).and_then(|v| v.as_i64()).unwrap();
    let sys_h = ctx.systems.get_or_spawn(sec_sys);
    let lethal = eve_destiny::DamageVector { em: 100_000.0, thermal: 100_000.0, kinetic: 100_000.0, explosive: 100_000.0 };
    sys_h.apply_damage_with_attacker(ambient_id, lethal, Some(char_id)).await.unwrap().unwrap();
    let after_ambient = ctx.db.with_snapshot(|s| eve_server::services::mission_fixture::get(s, char_id, chosen_mid).unwrap().npcs_remaining);
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
    let defer_sec = ctx
        .dispatch_rpc(
            &mut sess,
            "agentMgr",
            Some(sec_oid),
            "DoAction",
            &[PyRep::Integer(10)],
            &kw,
            None,
        )
        .await
        .expect("Defer next security offer");
    assert_eq!(
        defer_sec.dict_get("state").and_then(PyRep::as_str),
        Some("Declined"),
        "expected Deferred/Declined state on sec offer: {defer_sec:?}"
    );

    // 2b. Connections V (3359) lifts positive standing above 1.0 to unlock Level 2 agent, plus Defer/Decline/Accept/Quit
    let l2_same_fac = ctx
        .sde
        .agents
        .values()
        .filter(|a| {
            a.level == 2
                && a.agent_type_id == 2
                && a.station_id.is_some()
                && a.solar_system_id.is_some()
                && is_empire_agent(a)
                && ctx
                    .sde
                    .npc_corporations
                    .get(&a.corporation_id)
                    .and_then(|c| c.faction_id)
                    == Some(fac_id as u32)
        })
        .min_by_key(|a| a.agent_id)
        .unwrap_or(&l2_agent)
        .clone();
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
    let dist_agent = ctx
        .sde
        .agents
        .values()
        .filter(|a| {
            a.level == 1
                && a.division_id == 22
                && a.agent_type_id == 2
                && a.station_id.is_some()
                && a.solar_system_id.is_some()
                && is_empire_agent(a)
        })
        .min_by_key(|a| a.agent_id)
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
        let defer_dist = ctx
            .dispatch_rpc(
                &mut sess,
                "agentMgr",
                Some(dist_oid),
                "DoAction",
                &[PyRep::Integer(10)],
                &kw,
                None,
            )
            .await
            .expect("Defer non-remote courier offer");
        assert_eq!(
            defer_dist.dict_get("state").and_then(PyRep::as_str),
            Some("Declined"),
            "expected Deferred/Declined state on courier offer: {defer_dist:?}"
        );
    }
    assert!(courier_dest > 0, "Expected courier mission with remote destinationStationID");
    ctx.dispatch_rpc(&mut sess, "agentMgr", Some(dist_oid), "DoAction", &[PyRep::Integer(3)], &kw, None).await.expect("Accept courier mission");
    let not_at_dest = ctx.dispatch_rpc(&mut sess, "agentMgr", Some(dist_oid), "DoAction", &[PyRep::Integer(6)], &kw, None).await.unwrap_err();
    assert_eq!(not_at_dest.code, "CourierNotAtDestinationStation");

    let courier_iid = ctx.db.with_snapshot(|s| {
        s.item_custom_names
            .iter()
            .filter(|(iid, tag)| tag.starts_with(&format!("courier_cargo:{char_id}:")) && s.items.contains_key(iid))
            .min_by_key(|(iid, _)| *iid)
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

#[tokio::test]
async fn test_issue_766_agent_dialogue_v24_machonet_wire_contract() {
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let char_id = 90_000_001_i64;
    let mut sess = auth_pilot_harness(&ctx, 2766, "demo", "demo-secret", char_id, false)
        .await
        .into_transport()
        .inner
        .into_session();
    let kw = HashMap::new();

    let agents_rep = ctx
        .dispatch_rpc(&mut sess, "agentMgr", None, "GetAgents", &[PyRep::Integer(1), PyRep::Integer(24)], &kw, None)
        .await
        .expect("GetAgents");
    let PyRep::Rowset { rows, .. } = agents_rep else {
        panic!("Expected Rowset");
    };
    let row = rows
        .iter()
        .find(|r| r[5].as_i64() == Some(30_000_142) && r[6].as_i64().is_some())
        .unwrap_or(&rows[0]);
    let agent_id = row[0].as_i64().unwrap();
    let corp_id = row[3].as_i64().unwrap();
    let sys_id = row[5].as_i64().unwrap() as u32;
    let station_id = row[6].as_i64();

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.standings.insert(corp_id, 0.0);
        }
    });
    relocate_docked_pilot(&ctx, &mut sess, char_id, sys_id, station_id).await;

    let agent_oid = bind_service_oid(&ctx, &mut sess, "agentMgr", PyRep::Integer(agent_id)).await;

    // Before any mission exists, V24 machoNet calls to briefing/objective return None so
    // InteractWithAgent still reaches ConstructBrowsers() and creates self.briefingBrowser.
    let no_brief = ctx
        .dispatch_rpc(&mut sess, "agentMgr", Some(agent_oid), "GetMissionBriefingInfo", &[], &kw, Some("machoNet:26000"))
        .await
        .expect("GetMissionBriefingInfo when no mission active");
    assert_eq!(no_brief, PyRep::None);

    let no_obj = ctx
        .dispatch_rpc(&mut sess, "agentMgr", Some(agent_oid), "GetMissionObjectiveInfo", &[], &kw, Some("machoNet:26000"))
        .await
        .expect("GetMissionObjectiveInfo when no mission active");
    assert_eq!(no_obj, PyRep::None);

    // GetAgentLocationWrap returns a Dict with solarsystemID, locationID, and station typeID.
    let loc_wrap = ctx
        .dispatch_rpc(&mut sess, "agentMgr", Some(agent_oid), "GetAgentLocationWrap", &[], &kw, Some("machoNet:26000"))
        .await
        .expect("GetAgentLocationWrap");
    assert!(matches!(loc_wrap, PyRep::Dict(_)));
    assert_eq!(loc_wrap.dict_get("solarsystemID"), Some(&PyRep::Integer(sys_id as i64)));
    assert!(loc_wrap.dict_get("locationID").and_then(|v| v.as_i64()).is_some());
    assert!(loc_wrap.dict_get("typeID").and_then(|v| v.as_i64()).is_some_and(|t| t > 0));

    // DoAction over machoNet:26000 returns ((agentSaysMsg, availableActions), lastActionInfo).
    let do_action = ctx
        .dispatch_rpc(&mut sess, "agentMgr", Some(agent_oid), "DoAction", &[PyRep::None], &kw, Some("machoNet:26000"))
        .await
        .expect("DoAction(None)");
    let PyRep::Tuple(outer) = do_action else {
        panic!("DoAction on machoNet:26000 must return 2-tuple");
    };
    let PyRep::Tuple(inner) = &outer[0] else {
        panic!("first item must be (agentSaysMsg, availableActions)");
    };
    assert!(matches!(inner[0], PyRep::Tuple(ref m) if m.len() == 2));
    assert!(matches!(inner[1], PyRep::List(ref actions) if !actions.is_empty()));
    assert!(matches!(outer[1], PyRep::Dict(_)));
    assert_eq!(outer[1].dict_get("missionCompleted"), Some(&PyRep::Bool(false)));
    assert_eq!(outer[1].dict_get("missionDeclined"), Some(&PyRep::Bool(false)));
    assert_eq!(outer[1].dict_get("missionQuit"), Some(&PyRep::Bool(false)));
    assert_eq!(outer[1].dict_get("missionCantReplay"), Some(&PyRep::None));

    // Briefing & Objective info after offer return V24 Dict contracts.
    let brief = ctx
        .dispatch_rpc(&mut sess, "agentMgr", Some(agent_oid), "GetMissionBriefingInfo", &[], &kw, Some("machoNet:26000"))
        .await
        .expect("GetMissionBriefingInfo");
    assert!(matches!(brief, PyRep::Dict(_)));
    assert!(brief.dict_get("Mission Title ID").is_some());
    assert!(brief.dict_get("ContentID").is_some());
    let v24_briefing = brief
        .dict_get("Mission Briefing ID")
        .and_then(|v| v.as_str())
        .expect("Mission Briefing ID string");
    assert!(!v24_briefing.is_empty());
    assert!(
        !v24_briefing.contains("Execute objectives in system"),
        "V24 Mission Briefing ID must not emit fixed template: {v24_briefing}"
    );
    if let Some(mid) = brief.dict_get("ContentID").and_then(|v| v.as_i64()) {
        if let Some(title) = ctx.sde.missions.get(&(mid as u32)).map(|m| m.title.as_str()).filter(|s| !s.is_empty()) {
            assert!(
                v24_briefing.contains(title),
                "V24 Mission Briefing ID must include mission title {title:?}: {v24_briefing}"
            );
        }
    }

    let obj = ctx
        .dispatch_rpc(&mut sess, "agentMgr", Some(agent_oid), "GetMissionObjectiveInfo", &[], &kw, Some("machoNet:26000"))
        .await
        .expect("GetMissionObjectiveInfo");
    assert!(matches!(obj, PyRep::Dict(_)));
    assert_eq!(obj.dict_get("completionStatus"), Some(&PyRep::Integer(0)));
    assert!(obj.dict_get("normalRewards").is_some());

    // Accept returns ((agentSaysMsg, [(6, 6), (11, 11)]), lastActionInfo).
    let accepted = ctx
        .dispatch_rpc(&mut sess, "agentMgr", Some(agent_oid), "DoAction", &[PyRep::Integer(3)], &kw, Some("machoNet:26000"))
        .await
        .expect("DoAction(Accept)");
    let PyRep::Tuple(acc_outer) = accepted else { panic!("tuple") };
    let PyRep::Tuple(acc_inner) = &acc_outer[0] else { panic!("tuple") };
    assert!(matches!(acc_inner[1], PyRep::List(ref a) if a.len() == 2));
}

#[tokio::test]
async fn test_mission_complete_lp_isk_formula_and_plan_e5_end_to_end() {
    // parity: feat:pve.mission-complete, feat:pve.mission-lp-isk-reward-formula, plan:E-5
    eve_testkit::attest(&[
        "feat:pve.mission-complete",
        "feat:pve.mission-lp-isk-reward-formula",
        "plan:E-5",
    ]);

    use eve_server::services::mission_fixture::{self, MissionFixture};

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let char_id = 96_005_099_i64;
    ctx.db.provision_gm_account("e5rewards", "e5-rew-sec", char_id, "E5 Rewards Pilot");
    let kw = HashMap::new();
    let mut sess = auth_pilot_harness(&ctx, 9699, "e5rewards", "e5-rew-sec", char_id, true)
        .await
        .into_transport()
        .inner
        .into_session();

    let ship_id = ctx.db.with_snapshot(|s| s.characters[&char_id].active_ship_id);
    // Find an L1 Security agent in high-sec and an L1 Security agent in low-sec to verify truesec (1.6288 - true_sec)
    let hisec_l1 = ctx
        .sde
        .agents
        .values()
        .filter(|a| {
            a.level == 1
                && a.division_id == 24
                && a.agent_type_id == 2
                && a.station_id.is_some()
                && a.solar_system_id.is_some_and(|sid| {
                    ctx.sde
                        .solar_systems
                        .get(&sid)
                        .is_some_and(|ss| ss.security_status >= 0.70)
                })
        })
        .min_by_key(|a| a.agent_id)
        .expect("High-sec L1 security agent")
        .clone();
    let lowsec_l1 = ctx
        .sde
        .agents
        .values()
        .filter(|a| {
            a.level == 1
                && a.division_id == 24
                && a.agent_type_id == 2
                && a.station_id.is_some()
                && a.solar_system_id.is_some_and(|sid| {
                    ctx.sde
                        .solar_systems
                        .get(&sid)
                        .is_some_and(|ss| (0.05..=0.45).contains(&ss.security_status))
                })
        })
        .min_by_key(|a| a.agent_id)
        .expect("Low-sec L1 security agent")
        .clone();

    let hi_sys = hisec_l1.solar_system_id.unwrap();
    let hi_st = hisec_l1.station_id.unwrap();
    let hi_sec = ctx.sde.solar_systems[&hi_sys].security_status;
    let lo_sys = lowsec_l1.solar_system_id.unwrap();
    let lo_st = lowsec_l1.station_id.unwrap();
    let lo_sec = ctx.sde.solar_systems[&lo_sys].security_status;

    // Seed Negotiation 4 (+20% ISK & bonus), Security Connections 3 (+30% LP), plus Distribution 5 / Mining 5
    // to prove division Connections ignores mismatched skills.
    crate::common::slash_cmd(&ctx, &mut sess, "/giveskill 3356 4").await;
    crate::common::slash_cmd(&ctx, &mut sess, "/giveskill 3895 3").await;
    crate::common::slash_cmd(&ctx, &mut sess, "/giveskill 3894 5").await;
    crate::common::slash_cmd(&ctx, &mut sess, "/giveskill 3893 5").await;

    // Helper to seed an offered WikiCatalog mission & drive Accept -> Objective -> Kill -> Complete via RPCs
    dock_pilot_at_station(&ctx, &mut sess, char_id, ship_id, hi_sys, hi_st).await;
    let hi_oid = bind_service_oid(&ctx, &mut sess, "agentMgr", PyRep::Integer(hisec_l1.agent_id)).await;

    // 1. Complete Wiki L1 Denial of Reconnaissance (13_777: 67,000 ISK, 64,000 bonus, 3,600s, 73 base LP) in high-sec within window
    let now0 = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.balance = IskCents::from_isk(5_000_000);
        }
        mission_fixture::insert(
            snap,
            MissionFixture::offered(char_id, 13_777, hisec_l1.agent_id, hi_sys)
                .current_room_id(1)
                .npcs_remaining(1)
                .offered_at_unix(now0)
                .offer_expiry_unix(now0 + 86_400)
                .offered_level(Some(1)),
        );
    });

    let acc_denial = ctx
        .dispatch_rpc(&mut sess, "agentMgr", Some(hi_oid), "DoAction", &[PyRep::Integer(3), PyRep::Integer(13_777)], &kw, None)
        .await
        .expect("Accept 13_777");
    assert_eq!(acc_denial.dict_get("bonusWindowSecs"), Some(&PyRep::Integer(3_600)));
    assert_eq!(acc_denial.dict_get("bonusExpiryUnix"), Some(&PyRep::Integer((now0 + 3_600) as i64)));

    // Kill the spawned mission NPCs in space via system actor
    let denial_balls = ctx.db.with_snapshot(|s| mission_fixture::get(s, char_id, 13_777).unwrap().spawned_npc_ball_ids);
    assert!(!denial_balls.is_empty());
    let hi_handle = ctx.systems.get_or_spawn(hi_sys);
    let lethal = eve_destiny::DamageVector { em: 100_000.0, thermal: 100_000.0, kinetic: 100_000.0, explosive: 100_000.0 };
    for bid in denial_balls {
        hi_handle.apply_damage_with_attacker(bid, lethal, Some(char_id)).await.unwrap().unwrap();
    }

    let bal_before_denial = ctx.db.with_snapshot(|s| s.characters[&char_id].balance.raw_cents());
    let lp_before_denial = ctx.db.with_snapshot(|s| s.characters[&char_id].loyalty_points.get(&hisec_l1.corporation_id).copied().unwrap_or(0));
    let j_len_before = ctx.db.with_snapshot(|s| s.wallet_journal.len());

    let done_denial = ctx
        .dispatch_rpc(&mut sess, "agentMgr", Some(hi_oid), "DoAction", &[PyRep::Integer(6), PyRep::Integer(13_777)], &kw, None)
        .await
        .expect("Complete 13_777");

    // 67,000 * 1.20 = 80,400 ISK; 64,000 * 1.20 = 76,800 bonus; 73 * (1.6288 - hi_sec) * 1.30 LP
    let exp_denial_lp = (73.0 * (1.6288 - hi_sec).max(0.01) * 1.30).round() as i64;
    assert_eq!(done_denial.dict_get("iskReward"), Some(&PyRep::Integer(80_400)));
    assert_eq!(done_denial.dict_get("bonusReward"), Some(&PyRep::Integer(76_800)));
    assert_eq!(done_denial.dict_get("lpReward"), Some(&PyRep::Integer(exp_denial_lp)));
    assert_eq!(done_denial.dict_get("rewardItemID"), Some(&PyRep::None));

    let bal_after_denial = ctx.db.with_snapshot(|s| s.characters[&char_id].balance.raw_cents());
    let lp_after_denial = ctx.db.with_snapshot(|s| s.characters[&char_id].loyalty_points[&hisec_l1.corporation_id]);
    assert_eq!(bal_after_denial - bal_before_denial, (80_400 + 76_800) * 100);
    assert_eq!(lp_after_denial - lp_before_denial, exp_denial_lp);

    ctx.db.with_snapshot(|s| {
        let entries: Vec<_> = s.wallet_journal.iter().skip(j_len_before).collect();
        assert!(entries.iter().any(|e| e.entry_type == "agent_mission_reward" && e.amount == IskCents::from_isk(80_400)));
        assert!(entries.iter().any(|e| e.entry_type == "agent_mission_time_bonus_reward" && e.amount == IskCents::from_isk(76_800)));
    });

    // 2. Complete Patriotic Measures (4 of 5) (13_514: 29,000 ISK, 37,000 bonus, 3,840s (64m), 35 base LP) in low-sec
    //    AFTER advancing past 3,840s -> verifies distinct L1 reward, low-sec LP boost, and expired bonus zeroing
    dock_pilot_at_station(&ctx, &mut sess, char_id, ship_id, lo_sys, lo_st).await;
    let lo_oid = bind_service_oid(&ctx, &mut sess, "agentMgr", PyRep::Integer(lowsec_l1.agent_id)).await;
    let now1 = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        mission_fixture::insert(
            snap,
            MissionFixture::offered(char_id, 13_514, lowsec_l1.agent_id, lo_sys)
                .current_room_id(1)
                .offered_at_unix(now1)
                .offer_expiry_unix(now1 + 86_400)
                .offered_level(Some(1)),
        );
    });

    let acc_pat = ctx
        .dispatch_rpc(&mut sess, "agentMgr", Some(lo_oid), "DoAction", &[PyRep::Integer(3), PyRep::Integer(13_514)], &kw, None)
        .await
        .expect("Accept 13_514");
    assert_eq!(acc_pat.dict_get("bonusWindowSecs"), Some(&PyRep::Integer(3_840)));

    ctx.advance_secs(3_841);
    let obj_exp = ctx
        .dispatch_rpc(&mut sess, "agentMgr", Some(lo_oid), "GetMissionObjectiveInfo", &[PyRep::Integer(lowsec_l1.agent_id), PyRep::Integer(13_514)], &kw, None)
        .await
        .expect("GetMissionObjectiveInfo expired");
    assert_eq!(obj_exp.dict_get("bonusRewards"), Some(&PyRep::List(Vec::new())));

    let bal_before_pat = ctx.db.with_snapshot(|s| s.characters[&char_id].balance.raw_cents());
    let lp_before_pat = ctx.db.with_snapshot(|s| s.characters[&char_id].loyalty_points.get(&lowsec_l1.corporation_id).copied().unwrap_or(0));
    let j_len_pat = ctx.db.with_snapshot(|s| s.wallet_journal.len());

    let done_pat = ctx
        .dispatch_rpc(&mut sess, "agentMgr", Some(lo_oid), "DoAction", &[PyRep::Integer(6), PyRep::Integer(13_514)], &kw, None)
        .await
        .expect("Complete expired 13_514");

    // 29,000 * 1.20 = 34,800 ISK; expired bonus = 0; 35 * (1.6288 - lo_sec) * 1.30 LP
    let exp_pat_lp = (35.0 * (1.6288 - lo_sec).max(0.01) * 1.30).round() as i64;
    assert_eq!(done_pat.dict_get("iskReward"), Some(&PyRep::Integer(34_800)));
    assert_eq!(done_pat.dict_get("bonusReward"), Some(&PyRep::Integer(0)));
    assert_eq!(done_pat.dict_get("lpReward"), Some(&PyRep::Integer(exp_pat_lp)));
    assert!(exp_pat_lp > (35.0 * (1.6288 - hi_sec).max(0.01) * 1.30).round() as i64, "Low-sec true security must yield more LP per base LP than high-sec");

    let bal_after_pat = ctx.db.with_snapshot(|s| s.characters[&char_id].balance.raw_cents());
    let lp_after_pat = ctx.db.with_snapshot(|s| s.characters[&char_id].loyalty_points[&lowsec_l1.corporation_id]);
    assert_eq!(bal_after_pat - bal_before_pat, 34_800 * 100);
    assert_eq!(lp_after_pat - lp_before_pat, exp_pat_lp);

    ctx.db.with_snapshot(|s| {
        let entries: Vec<_> = s.wallet_journal.iter().skip(j_len_pat).collect();
        assert!(entries.iter().any(|e| e.entry_type == "agent_mission_reward" && e.amount == IskCents::from_isk(34_800)));
        assert!(!entries.iter().any(|e| e.entry_type == "agent_mission_time_bonus_reward"));
    });
}
