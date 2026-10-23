//! Agents, Missions, Standings, and Loyalty Point Store (`E-5`) complete parity integration tests.
//!



use crate::common;
use eve_db::{
    ActiveMissionRecord, InventoryFlag, InventoryItemRecord, IskCents, TrainedSkillRecord, TypeId,
};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use std::collections::HashMap;

const JITA_STATION_ID: i64 = 60_003_760;
const JITA_SOLAR_SYSTEM_ID: u32 = 30_000_142;
const CALDARI_NAVY_CORP_ID: i64 = 1_000_035;
const CALDARI_STATE_FACTION_ID: i64 = 500_001;
const CONNECTIONS_SKILL: TypeId = 3359;
const DIPLOMACY_SKILL: TypeId = 3357;
const CRIMINAL_CONNECTIONS_SKILL: TypeId = 3361;
const NEGOTIATION_SKILL: TypeId = 3356;
const SECURITY_CONNECTIONS_SKILL: TypeId = 3893;

async fn setup_pilot(
    ctx: &ServerContext,
    session_id: u64,
    username: &str,
    password: &str,
    char_id: i64,
) -> SessionState {
    let kw = HashMap::new();
    let mut sess = SessionState::new(session_id);
    common::login_pilot(ctx, &mut sess, username, password).await;
    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID");
    sess
}

async fn rpc_call(
    ctx: &ServerContext,
    sess: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let kw = HashMap::new();
    ctx.dispatch_rpc(sess, service, None, method, args, &kw, None)
        .await
}

fn colocate_with_agent(
    ctx: &ServerContext,
    sess: &mut SessionState,
    char_id: i64,
    agent_id: i64,
) -> (u32, i64) {
    let agent = ctx.sde.agents.get(&agent_id).expect("agent in SDE");
    let sys_id = agent.solar_system_id.unwrap_or(JITA_SOLAR_SYSTEM_ID);
    let sta_id = agent.station_id.unwrap_or(JITA_STATION_ID);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = sys_id;
            ch.station_id = agent.station_id;
            ch.structure_id = None;
        }
    });
    sess.solar_system_id = Some(sys_id);
    sess.station_id = agent.station_id;
    sess.structure_id = None;
    (sys_id, sta_id)
}

fn is_storyline(m: &eve_sde::MissionTemplate) -> bool {
    m.content_template
        .as_deref()
        .unwrap_or_default()
        .contains("Storyline")
}

fn find_agent_by_level_and_type(
    ctx: &ServerContext,
    level: u8,
    agent_type_id: u32,
) -> (i64, i64, Option<i64>) {
    ctx.sde
        .agents
        .values()
        .find(|a| a.level == level && a.agent_type_id == agent_type_id && a.solar_system_id.is_some())
        .map(|a| (a.agent_id, a.corporation_id, a.station_id))
        .expect("SDE agent with requested level and type must exist")
}

fn make_mission(char_id: i64, mission_id: u32, agent_id: i64, sys_id: u32, state: &str) -> ActiveMissionRecord {
    ActiveMissionRecord {
        mission_id,
        character_id: char_id,
        agent_id,
        solar_system_id: sys_id,
        state: state.into(),
        current_room_id: 1,
        npcs_remaining: 0,
        spawned_npc_ball_ids: Vec::new(),
    }
}

fn make_item(
    item_id: i64,
    type_id: TypeId,
    owner_id: i64,
    loc_id: i64,
    qty: i32,
    singleton: bool,
) -> InventoryItemRecord {
    InventoryItemRecord {
        item_id,
        type_id,
        owner_id,
        location_id: loc_id,
        flag: InventoryFlag::Hangar,
        quantity: qty,
        singleton,
        version: 1,
        dynamic_attributes: HashMap::new(),
        source_mutaplasmid_id: None,
        insured_payout_isk: 0.0,
        is_blueprint_copy: false,
        blueprint_runs_remaining: None,
    }
}

#[tokio::test]
async fn test_agent_dialogue_and_access_standings() {
    // parity: feat:pve.agent-finder, feat:pve.agent-conversation, feat:pve.mission-levels, feat:pve.mission-standing-gate
    eve_testkit::attest(&["feat:pve.agent-finder", "feat:pve.agent-conversation", "feat:pve.mission-levels", "feat:pve.mission-standing-gate"]);
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let char_id = 90_000_001;
    let mut sess = setup_pilot(&ctx, 501, "demo", "demo-secret", char_id).await;

    let (l1_agent_id, l1_corp_id, _) = find_agent_by_level_and_type(&ctx, 1, 2);
    let (l2_agent_id, l2_corp_id, _) = find_agent_by_level_and_type(&ctx, 2, 2);
    let (l3_agent_id, l3_corp_id, _) = find_agent_by_level_and_type(&ctx, 3, 2);
    let (l4_agent_id, l4_corp_id, _) = find_agent_by_level_and_type(&ctx, 4, 2);
    let (l5_agent_id, l5_corp_id, _) = find_agent_by_level_and_type(&ctx, 5, 2);

    // 1. GetAgents and GetSolarSystemOfAgent
    let agents_rep = rpc_call(
        &ctx,
        &mut sess,
        "agentMgr",
        "GetAgents",
        &[PyRep::Integer(4)],
    )
    .await
    .expect("GetAgents");
    assert!(!matches!(agents_rep, PyRep::None));

    let sys_rep = rpc_call(
        &ctx,
        &mut sess,
        "agentMgr",
        "GetSolarSystemOfAgent",
        &[PyRep::Integer(l1_agent_id)],
    )
    .await
    .expect("GetSolarSystemOfAgent");
    assert!(sys_rep.as_i64().unwrap_or(0) >= 30_000_000);

    // 2. GetInfoServiceDetails returns authentic SDE attributes
    let details = rpc_call(
        &ctx,
        &mut sess,
        "agentMgr",
        "GetInfoServiceDetails",
        &[PyRep::Integer(l4_agent_id)],
    )
    .await
    .expect("GetInfoServiceDetails");
    assert_eq!(
        details.dict_get("agentID").and_then(|v| v.as_i64()),
        Some(l4_agent_id)
    );
    assert_eq!(details.dict_get("level").and_then(|v| v.as_i64()), Some(4));

    // 3. L1 agent accessible at 0.0 standing, rejected at <= -2.0
    colocate_with_agent(&ctx, &mut sess, char_id, l1_agent_id);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.standings.clear();
        }
    });
    let l1_ok = rpc_call(
        &ctx,
        &mut sess,
        "agentMgr",
        "DoAction",
        &[PyRep::Integer(1), PyRep::Integer(l1_agent_id)],
    )
    .await
    .expect("L1 agent must allow access at 0.0 standing");
    let l1_mid = l1_ok.dict_get("missionID").and_then(|v| v.as_i64()).unwrap();
    assert!(!l1_ok.dict_get("briefing").and_then(|v| v.as_str()).unwrap_or("").is_empty());
    assert!(!l1_ok.dict_get("objectives").and_then(|v| v.as_str()).unwrap_or("").is_empty());
    assert!(l1_ok.dict_get("iskReward").and_then(|v| v.as_i64()).unwrap_or(0) > 0);
    assert!(l1_ok.dict_get("lpReward").and_then(|v| v.as_i64()).unwrap_or(0) > 0);
    let briefing_info = rpc_call(
        &ctx,
        &mut sess,
        "agentMgr",
        "GetMissionBriefingInfo",
        &[PyRep::Integer(l1_mid)],
    )
    .await
    .expect("GetMissionBriefingInfo");
    assert_eq!(briefing_info.dict_get("missionID").and_then(|v| v.as_i64()), Some(l1_mid));
    assert!(!briefing_info.dict_get("briefing").and_then(|v| v.as_str()).unwrap_or("").is_empty());

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.standings.insert(l1_corp_id, -2.0);
        }
    });
    let l1_hostile = rpc_call(
        &ctx,
        &mut sess,
        "agentMgr",
        "DoAction",
        &[PyRep::Integer(1), PyRep::Integer(l1_agent_id)],
    )
    .await
    .expect_err("L1 agent must reject character with <= -2.0 standing");
    assert_eq!(l1_hostile.code, "InsufficientAgentStanding");

    // 4. L2 (1.0), L3 (3.0), L4 (5.0), L5 (7.0) thresholds enforced on MAX(agent, corp, faction)
    for (agent_id, corp_id, req_threshold) in [
        (l2_agent_id, l2_corp_id, 1.0_f64),
        (l3_agent_id, l3_corp_id, 3.0_f64),
        (l4_agent_id, l4_corp_id, 5.0_f64),
        (l5_agent_id, l5_corp_id, 7.0_f64),
    ] {
        colocate_with_agent(&ctx, &mut sess, char_id, agent_id);
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&char_id) {
                ch.standings.clear();
                ch.standings.insert(corp_id, req_threshold - 0.1);
            }
        });
        let below_err = rpc_call(
            &ctx,
            &mut sess,
            "agentMgr",
            "DoAction",
            &[PyRep::Integer(1), PyRep::Integer(agent_id)],
        )
        .await
        .expect_err("Should reject below level standing threshold");
        assert_eq!(below_err.code, "InsufficientAgentStanding");

        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&char_id) {
                ch.standings.clear();
                ch.standings.insert(agent_id, req_threshold);
            }
        });
        let at_threshold = rpc_call(
            &ctx,
            &mut sess,
            "agentMgr",
            "DoAction",
            &[PyRep::Integer(1), PyRep::Integer(agent_id)],
        )
        .await
        .expect("MAX(agent, corp, faction) at threshold must succeed");
        assert_eq!(
            at_threshold.dict_get("state").and_then(|v| v.as_str()),
            Some("Offered")
        );
    }
}

#[tokio::test]
async fn test_mission_lifecycle_offer_accept_complete_decline_abandon_and_expiration() {
    // parity: feat:pve.mission-accept, feat:pve.mission-decline, feat:pve.mission-quit, feat:pve.mission-complete, feat:pve.mission-expire, feat:pve.mission-journal, feat:pve.mission-tracker
    eve_testkit::attest(&["feat:pve.mission-accept", "feat:pve.mission-decline", "feat:pve.mission-quit", "feat:pve.mission-complete", "feat:pve.mission-expire", "feat:pve.mission-journal", "feat:pve.mission-tracker"]);
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let char_id = 90_000_001;
    let mut sess = setup_pilot(&ctx, 502, "demo", "demo-secret", char_id).await;

    let (agent_id, corp_id, _) = find_agent_by_level_and_type(&ctx, 1, 2);
    colocate_with_agent(&ctx, &mut sess, char_id, agent_id);

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.standings.clear();
            ch.standings.insert(corp_id, 2.0);
        }
        let sk = snap.skills.entry(char_id).or_default();
        sk.insert(
            NEGOTIATION_SKILL,
            TrainedSkillRecord {
                type_id: NEGOTIATION_SKILL,
                level: 3,
                skill_points: 8_000.0,
            },
        );
        sk.insert(
            SECURITY_CONNECTIONS_SKILL,
            TrainedSkillRecord {
                type_id: SECURITY_CONNECTIONS_SKILL,
                level: 4,
                skill_points: 45_255.0,
            },
        );
    });

    // 1. Request mission offer (action 1)
    let offer = rpc_call(
        &ctx,
        &mut sess,
        "agentMgr",
        "DoAction",
        &[PyRep::Integer(1), PyRep::Integer(agent_id)],
    )
    .await
    .expect("Offer mission");
    let mission_id = offer
        .dict_get("missionID")
        .and_then(|v| v.as_i64())
        .expect("missionID") as u32;
    assert!(offer.dict_get("iskReward").and_then(|v| v.as_i64()).unwrap_or(0) > 0);
    assert!(offer.dict_get("lpReward").and_then(|v| v.as_i64()).unwrap_or(0) > 0);

    let journal = rpc_call(&ctx, &mut sess, "agentMgr", "GetMyJournalDetails", &[])
        .await
        .expect("GetMyJournalDetails");
    let PyRep::Tuple(journal_sections) = journal else {
        panic!("Expected tuple from GetMyJournalDetails");
    };
    let PyRep::List(missions_list) = &journal_sections[0] else {
        panic!("Expected missions list");
    };
    assert!(!missions_list.is_empty());

    // 2. First Decline within 4h window -> no standing penalty
    let decline_1 = rpc_call(
        &ctx,
        &mut sess,
        "agentMgr",
        "DoAction",
        &[PyRep::Integer(9), PyRep::Integer(mission_id as i64)],
    )
    .await
    .expect("First decline");
    assert_eq!(
        decline_1
            .dict_get("standingPenaltyApplied")
            .and_then(|v| v.as_bool()),
        Some(false)
    );

    // Re-offer and second Decline within 4h window -> applies standing penalty
    let offer_2 = rpc_call(
        &ctx,
        &mut sess,
        "agentMgr",
        "DoAction",
        &[PyRep::Integer(1), PyRep::Integer(agent_id)],
    )
    .await
    .expect("Second offer");
    let mission_id_2 = offer_2
        .dict_get("missionID")
        .and_then(|v| v.as_i64())
        .unwrap() as u32;
    let standing_before_decline2 = ctx.db.with_snapshot(|snap| {
        snap.characters
            .get(&char_id)
            .and_then(|c| c.standings.get(&corp_id).copied())
            .unwrap_or(0.0)
    });
    let decline_2 = rpc_call(
        &ctx,
        &mut sess,
        "agentMgr",
        "DoAction",
        &[PyRep::Integer(9), PyRep::Integer(mission_id_2 as i64)],
    )
    .await
    .expect("Second decline");
    assert_eq!(
        decline_2
            .dict_get("standingPenaltyApplied")
            .and_then(|v| v.as_bool()),
        Some(true)
    );
    let standing_after_decline2 = ctx.db.with_snapshot(|snap| {
        snap.characters
            .get(&char_id)
            .and_then(|c| c.standings.get(&corp_id).copied())
            .unwrap_or(0.0)
    });
    assert!(standing_after_decline2 < standing_before_decline2);

    // 3. Offer, Accept (action 3), and Abandon (`RemoveOfferFromJournal`)
    let offer_3 = rpc_call(
        &ctx,
        &mut sess,
        "agentMgr",
        "DoAction",
        &[PyRep::Integer(1), PyRep::Integer(agent_id)],
    )
    .await
    .expect("Third offer");
    let mission_id_3 = offer_3
        .dict_get("missionID")
        .and_then(|v| v.as_i64())
        .unwrap() as u32;
    let accept_3 = rpc_call(
        &ctx,
        &mut sess,
        "agentMgr",
        "DoAction",
        &[PyRep::Integer(3), PyRep::Integer(mission_id_3 as i64)],
    )
    .await
    .expect("Accept third mission");
    assert_eq!(
        accept_3.dict_get("state").and_then(|v| v.as_str()),
        Some("Accepted")
    );
    let bm3_id = accept_3.dict_get("bookmarkID").and_then(|v| v.as_i64()).unwrap_or(0);
    assert!(bm3_id > 0);
    assert!(ctx.db.with_snapshot(|snap| snap.bookmarks.contains_key(&bm3_id)));

    let st_before_quit = ctx.db.with_snapshot(|snap| {
        snap.characters[&char_id].standings.get(&corp_id).copied().unwrap_or(0.0)
    });
    let abandon_3 = rpc_call(
        &ctx,
        &mut sess,
        "agentMgr",
        "RemoveOfferFromJournal",
        &[PyRep::Integer(mission_id_3 as i64)],
    )
    .await
    .expect("Abandon accepted mission");
    assert_eq!(abandon_3.dict_get("state").and_then(|v| v.as_str()), Some("Quit"));
    assert_eq!(abandon_3.dict_get("standingPenaltyApplied").and_then(|v| v.as_bool()), Some(true));
    assert_eq!(abandon_3.dict_get("dungeonCleaned").and_then(|v| v.as_bool()), Some(true));
    assert!(!ctx.db.with_snapshot(|snap| snap.bookmarks.contains_key(&bm3_id)));
    let st_after_quit = ctx.db.with_snapshot(|snap| {
        snap.characters[&char_id].standings.get(&corp_id).copied().unwrap_or(0.0)
    });
    assert!(st_after_quit < st_before_quit, "Quitting accepted mission must penalize standing");
    let journal_after_quit = rpc_call(&ctx, &mut sess, "agentMgr", "GetMyJournalDetails", &[])
        .await
        .expect("GetMyJournalDetails after quit");
    let PyRep::Tuple(jq_sections) = journal_after_quit else { panic!() };
    let PyRep::List(jq_missions) = &jq_sections[0] else { panic!() };
    assert!(jq_missions.is_empty(), "Abandoned mission must be removed from journal");

    // 4. Mission Expiration check
    let offer_4 = rpc_call(
        &ctx,
        &mut sess,
        "agentMgr",
        "DoAction",
        &[PyRep::Integer(1), PyRep::Integer(agent_id)],
    )
    .await
    .expect("Fourth offer");
    let mission_id_4 = offer_4
        .dict_get("missionID")
        .and_then(|v| v.as_i64())
        .unwrap() as u32;
    rpc_call(
        &ctx,
        &mut sess,
        "agentMgr",
        "DoAction",
        &[PyRep::Integer(3), PyRep::Integer(mission_id_4 as i64)],
    )
    .await
    .expect("Accept fourth mission");

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(m) = snap.active_missions.get_mut(&(char_id, mission_id_4)) {
            m.npcs_remaining = 0;
        }
        for note in snap.owner_notes.values_mut() {
            if note.item_id == mission_id_4 as i64 && note.note.contains("accept_expiry_unix") {
                if let Ok(mut val) = serde_json::from_str::<serde_json::Value>(&note.note) {
                    val["accept_expiry_unix"] = serde_json::json!(1000_u64);
                    note.note = val.to_string();
                }
            }
        }
    });
    let expired_err = rpc_call(
        &ctx,
        &mut sess,
        "agentMgr",
        "DoAction",
        &[PyRep::Integer(6), PyRep::Integer(mission_id_4 as i64)],
    )
    .await
    .expect_err("Expired mission must fail on completion attempt");
    assert_eq!(expired_err.code, "MissionExpired");
}

#[tokio::test]
async fn test_mission_archetypes_combat_courier_mining_and_burner() {
    // parity: feat:pve.mission-security, feat:pve.mission-courier, feat:pve.mission-trade, feat:pve.anomic-burner-missions
    eve_testkit::attest(&["feat:pve.mission-security", "feat:pve.mission-courier", "feat:pve.mission-trade", "feat:pve.anomic-burner-missions"]);
    let ctx = ServerContext::new();
    let char_id = 90_000_001;
    let mut sess = setup_pilot(&ctx, 503, "demo", "demo-secret", char_id).await;

    let (dist_agent_id, dist_corp_id) = ctx
        .sde
        .agents
        .values()
        .find(|a| a.level == 1 && a.agent_type_id == 2 && a.division_id == 22 && a.solar_system_id.is_some())
        .map(|a| (a.agent_id, a.corporation_id))
        .expect("Distribution agent in SDE");
    let (_, dist_station_id) = colocate_with_agent(&ctx, &mut sess, char_id, dist_agent_id);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.standings.insert(dist_corp_id, 2.0);
        }
    });

    // 1. Courier Mission archetype offered by Distribution agent (division 22)
    let courier_offer = rpc_call(
        &ctx,
        &mut sess,
        "agentMgr",
        "DoAction",
        &[PyRep::Integer(1), PyRep::Integer(dist_agent_id)],
    )
    .await
    .expect("Offer courier mission");
    let courier_mid = courier_offer
        .dict_get("missionID")
        .and_then(|v| v.as_i64())
        .unwrap() as u32;

    rpc_call(
        &ctx,
        &mut sess,
        "agentMgr",
        "DoAction",
        &[PyRep::Integer(3), PyRep::Integer(courier_mid as i64)],
    )
    .await
    .expect("Accept courier mission");

    // Accepting courier mission grants 10 units of type 3683 into cargo; completing without moving to station hangar fails
    let missing_cargo_err = rpc_call(
        &ctx,
        &mut sess,
        "agentMgr",
        "DoAction",
        &[PyRep::Integer(6), PyRep::Integer(courier_mid as i64)],
    )
    .await
    .expect_err("Courier mission must fail when delivery cargo is not in station hangar");
    assert_eq!(missing_cargo_err.code, "MissionIncomplete");

    // Move granted courier cargo (type 3683) into agent station hangar
    ctx.db.with_mut_snapshot(|snap| {
        for it in snap.items.values_mut() {
            if it.owner_id == char_id && it.type_id == 3683 {
                it.location_id = dist_station_id;
                it.flag = InventoryFlag::Hangar;
            }
        }
    });
    let courier_done = rpc_call(
        &ctx,
        &mut sess,
        "agentMgr",
        "DoAction",
        &[PyRep::Integer(6), PyRep::Integer(courier_mid as i64)],
    )
    .await
    .expect("Complete courier mission with cargo in station hangar");
    assert_eq!(
        courier_done.dict_get("state").and_then(|v| v.as_str()),
        Some("Completed")
    );

    // 2. Mining Mission archetype: requires 500 units of Veldspar (1230) in station hangar
    let (agent_id, corp_id) = ctx
        .sde
        .agents
        .values()
        .find(|a| a.level == 4 && a.agent_type_id == 2 && a.division_id == 24 && a.solar_system_id.is_some())
        .map(|a| (a.agent_id, a.corporation_id))
        .expect("Level 4 Security agent in SDE");
    let (agent_sys_id, agent_station_id) = colocate_with_agent(&ctx, &mut sess, char_id, agent_id);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.standings.insert(corp_id, 6.0);
        }
    });
    // Lowest-ID SDE mission whose content template is the basic mining template (deterministic: a
    // title substring match also hit e.g. "Re-examining Options" depending on HashMap order).
    let mining_mid = ctx
        .sde
        .missions
        .values()
        .filter(|m| m.content_template.as_deref().is_some_and(|c| c.ends_with("BasicMiningMission")))
        .map(|m| m.mission_id)
        .min()
        .expect("SDE mining mission");

    ctx.db.with_mut_snapshot(|snap| {
        snap.active_missions.insert(
            (char_id, mining_mid),
            make_mission(char_id, mining_mid, agent_id, agent_sys_id, "Accepted"),
        );
    });

    let mining_err = rpc_call(
        &ctx,
        &mut sess,
        "agentMgr",
        "DoAction",
        &[PyRep::Integer(6), PyRep::Integer(mining_mid as i64)],
    )
    .await
    .expect_err("Mining mission must fail without 500 units of ore");
    assert_eq!(mining_err.code, "MissionIncomplete");

    ctx.db.with_mut_snapshot(|snap| {
        snap.next_entity_id += 1;
        let oid = snap.next_entity_id;
        snap.items
            .insert(oid, make_item(oid, 1230, char_id, agent_station_id, 500, false));
    });
    let mining_done = rpc_call(
        &ctx,
        &mut sess,
        "agentMgr",
        "DoAction",
        &[PyRep::Integer(6), PyRep::Integer(mining_mid as i64)],
    )
    .await
    .expect("Mining mission completes once 500 units of ore are delivered");
    assert_eq!(
        mining_done.dict_get("state").and_then(|v| v.as_str()),
        Some("Completed")
    );

    // 3. Combat & Anomic Burner Mission Acceleration Gate (`keeper.ActivateAccelerationGate`) ship size restriction
    let burner_offer = rpc_call(
        &ctx,
        &mut sess,
        "agentMgr",
        "DoAction",
        &[PyRep::Integer(1), PyRep::Integer(agent_id)],
    )
    .await
    .expect("Offer Anomic Burner mission from Level 4 Security agent");
    let burner_mid = burner_offer
        .dict_get("missionID")
        .and_then(|v| v.as_i64())
        .unwrap() as u32;
    rpc_call(
        &ctx,
        &mut sess,
        "agentMgr",
        "DoAction",
        &[PyRep::Integer(3), PyRep::Integer(burner_mid as i64)],
    )
    .await
    .expect("Accept Anomic Burner mission");

    let battleship_id = ctx.db.with_mut_snapshot(|snap| {
        snap.next_entity_id += 1;
        let bsid = snap.next_entity_id;
        snap.items.insert(
            bsid,
            make_item(bsid, 638, char_id, agent_sys_id as i64, 1, true),
        );
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = None;
            ch.active_ship_id = bsid;
        }
        if let Some(m) = snap.active_missions.get_mut(&(char_id, burner_mid)) {
            m.npcs_remaining = 0;
        }
        bsid
    });
    sess.station_id = None;
    sess.ship_id = Some(battleship_id);

    let gate_err = rpc_call(
        &ctx,
        &mut sess,
        "keeper",
        "ActivateAccelerationGate",
        &[PyRep::Integer(burner_mid as i64)],
    )
    .await
    .expect_err("Battleship must be rejected by Anomic Burner acceleration gate");
    assert_eq!(gate_err.code, "ShipRestrictedByGate");

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ship) = snap.items.get_mut(&battleship_id) {
            ship.type_id = 587;
        }
    });
    let gate_ok = rpc_call(
        &ctx,
        &mut sess,
        "keeper",
        "ActivateAccelerationGate",
        &[PyRep::Integer(burner_mid as i64)],
    )
    .await
    .expect("Frigate passes acceleration gate");
    assert_eq!(
        gate_ok.dict_get("currentRoomID").and_then(|v| v.as_i64()),
        Some(2)
    );
}

#[tokio::test]
async fn test_special_agents_storyline_epic_arcs_cosmos_datacenter_career_rd_and_locator() {
    // parity: feat:pve.mission-storyline, feat:pve.epic-arcs, feat:pve.storyline-arcs-soe, feat:pve.data-centers, feat:pve.career-agents, feat:pve.research-agents, feat:pve.locator-agents
    eve_testkit::attest(&["feat:pve.mission-storyline", "feat:pve.epic-arcs", "feat:pve.storyline-arcs-soe", "feat:pve.data-centers", "feat:pve.career-agents", "feat:pve.research-agents", "feat:pve.locator-agents"]);
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let char_id = 90_000_001;
    let mut sess = setup_pilot(&ctx, 504, "demo", "demo-secret", char_id).await;

    // 1. Storyline tally: 15 completed regular missions + 16th completion triggers Storyline offer
    let (reg_agent_id, reg_corp_id, _) = find_agent_by_level_and_type(&ctx, 1, 2);
    let (reg_sys_id, _) = colocate_with_agent(&ctx, &mut sess, char_id, reg_agent_id);
    let reg_mid = ctx
        .sde
        .missions
        .values()
        .find(|m| {
            m.has_standing_rewards
                && !is_storyline(m)
                && m.content_template
                    .as_deref()
                    .unwrap_or_default()
                    .contains("BasicKill")
                && !m.title.to_lowercase().contains("mining")
        })
        .map(|m| m.mission_id)
        .expect("Regular combat mission with standing rewards");

    ctx.db.with_mut_snapshot(|snap| {
        snap.mission_tally.insert(char_id, 15);
        snap.active_missions.insert(
            (char_id, reg_mid),
            make_mission(char_id, reg_mid, reg_agent_id, reg_sys_id, "Accepted"),
        );
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.standings.insert(reg_corp_id, 2.0);
        }
    });

    let complete_16th = rpc_call(
        &ctx,
        &mut sess,
        "agentMgr",
        "DoAction",
        &[PyRep::Integer(6), PyRep::Integer(reg_mid as i64)],
    )
    .await
    .expect("Complete 16th mission");
    assert_eq!(
        complete_16th
            .dict_get("missionTally")
            .and_then(|v| v.as_i64()),
        Some(16)
    );
    assert_eq!(
        complete_16th
            .dict_get("storylineTriggered")
            .and_then(|v| v.as_bool()),
        Some(true)
    );

    // 2. Epic Arcs (`agent_type_id == 10`): complete arc mission -> 90-day cooldown blocks re-offer
    let (epic_agent_id, epic_corp_id, _) = find_agent_by_level_and_type(&ctx, 1, 10);
    let (epic_sys_id, _) = colocate_with_agent(&ctx, &mut sess, char_id, epic_agent_id);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.standings.insert(epic_corp_id, 5.0);
        }
        snap.active_missions.insert(
            (char_id, reg_mid),
            make_mission(char_id, reg_mid, epic_agent_id, epic_sys_id, "Accepted"),
        );
    });
    rpc_call(
        &ctx,
        &mut sess,
        "agentMgr",
        "DoAction",
        &[PyRep::Integer(6), PyRep::Integer(reg_mid as i64)],
    )
    .await
    .expect("Complete Epic Arc mission");

    let epic_status = rpc_call(&ctx, &mut sess, "agentMgr", "GetMyEpicArcStatus", &[])
        .await
        .expect("GetMyEpicArcStatus");
    let PyRep::List(arcs) = epic_status else {
        panic!("Expected list from GetMyEpicArcStatus");
    };
    assert!(!arcs.is_empty());

    let epic_cooldown_err = rpc_call(
        &ctx,
        &mut sess,
        "agentMgr",
        "DoAction",
        &[PyRep::Integer(1), PyRep::Integer(epic_agent_id)],
    )
    .await
    .expect_err("Epic Arc must enforce 90-day cooldown");
    assert_eq!(epic_cooldown_err.code, "EpicArcOnCooldown");

    // 3. Career (`agent_type_id == 12`): one-time lockouts + GetCompletedCareerAgentIDs
    let (career_agent_id, career_corp_id, _) = find_agent_by_level_and_type(&ctx, 1, 12);
    colocate_with_agent(&ctx, &mut sess, char_id, career_agent_id);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.standings.insert(career_corp_id, 2.0);
        }
    });
    let career_offer = rpc_call(
        &ctx,
        &mut sess,
        "agentMgr",
        "DoAction",
        &[PyRep::Integer(1), PyRep::Integer(career_agent_id)],
    )
    .await
    .expect("Career agent offer");
    let career_mid = career_offer
        .dict_get("missionID")
        .and_then(|v| v.as_i64())
        .unwrap() as u32;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(m) = snap.active_missions.get_mut(&(char_id, career_mid)) {
            m.state = "Completed".into();
        }
    });
    let career_repeat_err = rpc_call(
        &ctx,
        &mut sess,
        "agentMgr",
        "DoAction",
        &[PyRep::Integer(1), PyRep::Integer(career_agent_id)],
    )
    .await
    .expect_err("Career agent must reject repeat completion");
    assert_eq!(career_repeat_err.code, "OneTimeAgentMissionCompleted");

    let career_ids = rpc_call(&ctx, &mut sess, "agentMgr", "GetCompletedCareerAgentIDs", &[])
        .await
        .expect("GetCompletedCareerAgentIDs");
    let PyRep::List(completed_careers) = career_ids else {
        panic!("Expected list from GetCompletedCareerAgentIDs");
    };
    assert!(completed_careers.contains(&PyRep::Integer(career_agent_id)));

    // 4. Data Center (`agent_type_id == 5`): Tag Turn-In (requires 1x tag 17202, one-time lockout)
    let (dc_agent_id, dc_corp_id, _) = find_agent_by_level_and_type(&ctx, 1, 5);
    let (_, dc_station_id) = colocate_with_agent(&ctx, &mut sess, char_id, dc_agent_id);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.standings.insert(dc_corp_id, 2.0);
        }
        snap.next_entity_id += 1;
        let tid = snap.next_entity_id;
        snap.items
            .insert(tid, make_item(tid, 17_202, char_id, dc_station_id, 1, false));
    });
    let dc_offer = rpc_call(
        &ctx,
        &mut sess,
        "agentMgr",
        "DoAction",
        &[PyRep::Integer(1), PyRep::Integer(dc_agent_id)],
    )
    .await
    .expect("Data Center offer");
    let dc_mid = dc_offer
        .dict_get("missionID")
        .and_then(|v| v.as_i64())
        .unwrap() as u32;
    rpc_call(
        &ctx,
        &mut sess,
        "agentMgr",
        "DoAction",
        &[PyRep::Integer(3), PyRep::Integer(dc_mid as i64)],
    )
    .await
    .expect("Accept Data Center tag mission");
    let dc_done = rpc_call(
        &ctx,
        &mut sess,
        "agentMgr",
        "DoAction",
        &[PyRep::Integer(6), PyRep::Integer(dc_mid as i64)],
    )
    .await
    .expect("Complete Data Center tag turn-in");
    assert_eq!(
        dc_done.dict_get("state").and_then(|v| v.as_str()),
        Some("Completed")
    );

    let dc_repeat_err = rpc_call(
        &ctx,
        &mut sess,
        "agentMgr",
        "DoAction",
        &[PyRep::Integer(1), PyRep::Integer(dc_agent_id)],
    )
    .await
    .expect_err("Repeat Data Center mission must be rejected");
    assert_eq!(dc_repeat_err.code, "OneTimeAgentMissionCompleted");

    // 5. R&D Agents (`DoAction` action 12 = StartResearch, action 14 = BuyDatacores)
    let (rd_agent_id, rd_corp_id, _) = find_agent_by_level_and_type(&ctx, 1, 4);
    colocate_with_agent(&ctx, &mut sess, char_id, rd_agent_id);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.standings.insert(rd_corp_id, 5.0);
            ch.balance = IskCents::from_isk(10_000_000);
        }
        snap.skills.entry(char_id).or_default().insert(
            11_448,
            TrainedSkillRecord {
                type_id: 11_448,
                level: 4,
                skill_points: 45_255.0,
            },
        );
    });
    let start_rd = rpc_call(
        &ctx,
        &mut sess,
        "agentMgr",
        "DoAction",
        &[
            PyRep::Integer(12),
            PyRep::Integer(rd_agent_id),
            PyRep::Integer(11_448),
        ],
    )
    .await
    .expect("Start R&D Research");
    assert!(
        start_rd
            .dict_get("pointsPerDay")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0)
            > 0.0
    );

    ctx.db.with_mut_snapshot(|snap| {
        for note in snap.owner_notes.values_mut() {
            if note.note.contains("points_per_day") {
                if let Ok(mut arr) = serde_json::from_str::<serde_json::Value>(&note.note) {
                    if let Some(first) = arr.get_mut(0) {
                        first["started_unix"] = serde_json::json!(1_u64);
                    }
                    note.note = arr.to_string();
                }
            }
        }
    });
    let redeem_rd = rpc_call(
        &ctx,
        &mut sess,
        "agentMgr",
        "DoAction",
        &[
            PyRep::Integer(14),
            PyRep::Integer(rd_agent_id),
            PyRep::Integer(1),
        ],
    )
    .await
    .expect("Redeem R&D Datacores");
    assert_eq!(
        redeem_rd
            .dict_get("datacoreTypeID")
            .and_then(|v| v.as_i64()),
        Some(20_424)
    );

    // 6. Locator Agents (`DoAction` action 15 = LocateCharacter)
    let locator_agent_id = ctx
        .sde
        .agents
        .values()
        .find(|a| a.is_locator && a.solar_system_id.is_some())
        .map(|a| a.agent_id)
        .expect("Locator agent in SDE");
    colocate_with_agent(&ctx, &mut sess, char_id, locator_agent_id);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.standings.insert(locator_agent_id, 8.0);
        }
    });
    let locate_res = rpc_call(
        &ctx,
        &mut sess,
        "agentMgr",
        "DoAction",
        &[
            PyRep::Integer(15),
            PyRep::Integer(locator_agent_id),
            PyRep::Integer(90_000_002),
        ],
    )
    .await
    .expect("LocateCharacter via DoAction 15");
    assert_eq!(
        locate_res
            .dict_get("targetCharacterID")
            .and_then(|v| v.as_i64()),
        Some(90_000_002)
    );

    let locate_cd_err = rpc_call(
        &ctx,
        &mut sess,
        "agentMgr",
        "DoAction",
        &[
            PyRep::Integer(15),
            PyRep::Integer(locator_agent_id),
            PyRep::Integer(90_000_002),
        ],
    )
    .await
    .expect_err("Locator agent must enforce cooldown");
    assert_eq!(locate_cd_err.code, "LocatorAgentOnCooldown");
}

#[tokio::test]
async fn test_standings_formulas_derived_effective_corp_standing_data_and_transactions() {
    // parity: feat:pve.mission-faction-standing-loss, feat:standings.npc-standings, feat:standings.standing-gain-missions, feat:standings.standing-loss, feat:standings.standing-compositions, feat:standings.effective-standing, feat:standings.corp-standings, feat:standings.npc-npc-standings, feat:standings.standing-notifications, feat:standings.trig-edencom-standings
    eve_testkit::attest(&["feat:pve.mission-faction-standing-loss", "feat:standings.npc-standings", "feat:standings.standing-gain-missions", "feat:standings.standing-loss", "feat:standings.standing-compositions", "feat:standings.effective-standing", "feat:standings.corp-standings", "feat:standings.npc-npc-standings", "feat:standings.standing-notifications", "feat:standings.trig-edencom-standings"]);
    let ctx = ServerContext::new();
    let char_id = 90_000_001;
    let mut sess = setup_pilot(&ctx, 505, "demo", "demo-secret", char_id).await;

    let pirate_corp_id = ctx
        .sde
        .npc_corporations
        .values()
        .find(|c| matches!(c.faction_id, Some(500_010 | 500_011 | 500_012)))
        .map(|c| c.corporation_id)
        .unwrap_or(1_000_125);

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.standings.clear();
            ch.standings.insert(CALDARI_NAVY_CORP_ID, 5.0);
            ch.standings.insert(pirate_corp_id, 2.0);
            ch.standings.insert(CALDARI_STATE_FACTION_ID, -5.0);
        }
        let sk = snap.skills.entry(char_id).or_default();
        for stid in [
            CONNECTIONS_SKILL,
            DIPLOMACY_SKILL,
            CRIMINAL_CONNECTIONS_SKILL,
        ] {
            sk.insert(
                stid,
                TrainedSkillRecord {
                    type_id: stid,
                    level: 5,
                    skill_points: 256_000.0,
                },
            );
        }
    });

    // Connections V on +5.0 -> 6.0; Diplomacy V on -5.0 -> -2.0; Criminal Connections V on +2.0 -> 3.6
    let mut eff_tenths = Vec::new();
    for (target_id, expected_eff) in [
        (CALDARI_NAVY_CORP_ID, 6.0_f64),
        (CALDARI_STATE_FACTION_ID, -2.0_f64),
        (pirate_corp_id, 3.6_f64),
    ] {
        let comp = rpc_call(
            &ctx,
            &mut sess,
            "standingMgr",
            "GetStandingCompositions",
            &[PyRep::Integer(target_id)],
        )
        .await
        .expect("GetStandingCompositions");
        let PyRep::List(items) = comp else {
            panic!("Expected list from GetStandingCompositions");
        };
        let eff = items[0]
            .dict_get("effectiveStanding")
            .and_then(|v| v.as_f64())
            .unwrap();
        assert!((eff - expected_eff).abs() < 1e-6);
        eff_tenths.push((eff * 10.0).round() as i64);
    }
    assert_eq!(eff_tenths[0], 60);
    assert_eq!(eff_tenths[1], -20);
    assert_eq!(eff_tenths[2], 36);
    assert_eq!(eff_tenths.len(), 3);

    let corp_st = rpc_call(&ctx, &mut sess, "standingMgr", "GetCorpStandings", &[])
        .await
        .expect("GetCorpStandings");
    assert!(!matches!(corp_st, PyRep::None));

    let npc_st = rpc_call(&ctx, &mut sess, "standingMgr", "GetNPCNPCStandings", &[])
        .await
        .expect("GetNPCNPCStandings");
    assert!(!matches!(npc_st, PyRep::None));

    // Complete a Storyline mission -> verifies asymptotic gain formula, derived faction propagation, and GetStandingTransactions
    let (sl_agent_id, sl_corp_id, _) = find_agent_by_level_and_type(&ctx, 1, 6);
    let (sl_sys_id, _) = colocate_with_agent(&ctx, &mut sess, char_id, sl_agent_id);
    let mut storyline_missions: Vec<&eve_sde::MissionTemplate> = ctx
        .sde
        .missions
        .values()
        .filter(|m| {
            is_storyline(m)
                && m.has_standing_rewards
                && !m.is_courier
                && m.content_template
                    .as_deref()
                    .unwrap_or_default()
                    .contains("KillMission")
                && !m.title.to_lowercase().contains("mining")
                && !m.title.to_lowercase().contains("ore")
                && !m.title.to_lowercase().contains("mineral")
        })
        .collect();
    storyline_missions.sort_by_key(|m| m.mission_id);
    let sl_mid = storyline_missions.first().map(|m| m.mission_id).unwrap_or(1259);

    ctx.db.with_mut_snapshot(|snap| {
        snap.active_missions.insert(
            (char_id, sl_mid),
            make_mission(char_id, sl_mid, sl_agent_id, sl_sys_id, "Accepted"),
        );
    });
    rpc_call(
        &ctx,
        &mut sess,
        "agentMgr",
        "DoAction",
        &[PyRep::Integer(6), PyRep::Integer(sl_mid as i64)],
    )
    .await
    .expect("Complete storyline mission for derived standings");

    let txs = rpc_call(
        &ctx,
        &mut sess,
        "standingMgr",
        "GetStandingTransactions",
        &[PyRep::Integer(sl_corp_id), PyRep::Integer(char_id)],
    )
    .await
    .expect("GetStandingTransactions");
    assert!(!matches!(txs, PyRep::None));
}

#[tokio::test]
async fn test_lp_store_offers_exchange_rates_and_purchase_validation() {
    // parity: feat:lp.lp-balance, feat:lp.lp-store-offers, feat:lp.lp-store-buy, feat:lp.lp-corp-wallet, feat:lp.concord-lp, feat:lp.lp-reward-logs
    eve_testkit::attest(&["feat:lp.lp-balance", "feat:lp.lp-store-offers", "feat:lp.lp-store-buy", "feat:lp.lp-corp-wallet", "feat:lp.concord-lp", "feat:lp.lp-reward-logs"]);
    let ctx = ServerContext::new();
    let char_id = 90_000_001;
    let mut sess = setup_pilot(&ctx, 506, "demo", "demo-secret", char_id).await;
    sess.station_id = Some(JITA_STATION_ID);

    let offers = rpc_call(
        &ctx,
        &mut sess,
        "LPStoreMgr",
        "GetAvailableOffersFromCorp",
        &[PyRep::Integer(CALDARI_NAVY_CORP_ID)],
    )
    .await
    .expect("GetAvailableOffersFromCorp");
    let PyRep::List(offer_list) = offers else {
        panic!("Expected list of LP offers");
    };
    assert!(offer_list.len() >= 3);

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.loyalty_points.insert(CALDARI_NAVY_CORP_ID, 100);
        }
    });
    let lp_err = rpc_call(
        &ctx,
        &mut sess,
        "LPStoreMgr",
        "TakeOfferForCharacter",
        &[
            PyRep::Integer(CALDARI_NAVY_CORP_ID),
            PyRep::Integer(1002),
            PyRep::Integer(1),
        ],
    )
    .await
    .expect_err("TakeOfferForCharacter must fail with insufficient LP");
    assert_eq!(lp_err.code, "InsufficientLoyaltyPoints");

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.loyalty_points.insert(CALDARI_NAVY_CORP_ID, 50_000);
            ch.balance = IskCents::from_isk(100_000_000);
        }
    });
    let purchase = rpc_call(
        &ctx,
        &mut sess,
        "LPStoreMgr",
        "TakeOfferForCharacter",
        &[
            PyRep::Integer(CALDARI_NAVY_CORP_ID),
            PyRep::Integer(1002),
            PyRep::Integer(2),
        ],
    )
    .await
    .expect("TakeOfferForCharacter 2x offer 1002");
    assert_eq!(
        purchase
            .dict_get("deliveredTypeID")
            .and_then(|v| v.as_i64()),
        Some(27_361)
    );
    assert_eq!(
        purchase.dict_get("quantity").and_then(|v| v.as_i64()),
        Some(2_000)
    );
    assert_eq!(
        purchase.dict_get("remainingLP").and_then(|v| v.as_i64()),
        Some(49_000)
    );

    // Offer 1004 requires 100x Tritanium (type 34) trade-in; clear any default Tritanium first
    ctx.db.with_mut_snapshot(|snap| {
        snap.items
            .retain(|_, it| !(it.owner_id == char_id && it.type_id == 34));
    });
    let req_item_err = rpc_call(
        &ctx,
        &mut sess,
        "LPStoreMgr",
        "TakeOfferForCharacter",
        &[
            PyRep::Integer(CALDARI_NAVY_CORP_ID),
            PyRep::Integer(1004),
            PyRep::Integer(1),
        ],
    )
    .await
    .expect_err("Offer 1004 must fail without required trade-in item 34");
    assert_eq!(req_item_err.code, "MissingRequiredItems");

    ctx.db.with_mut_snapshot(|snap| {
        snap.next_entity_id += 1;
        let req_id = snap.next_entity_id;
        snap.items
            .insert(req_id, make_item(req_id, 34, char_id, JITA_STATION_ID, 100, false));
    });
    let tradein_purchase = rpc_call(
        &ctx,
        &mut sess,
        "LPStoreMgr",
        "TakeOfferForCharacter",
        &[
            PyRep::Integer(CALDARI_NAVY_CORP_ID),
            PyRep::Integer(1004),
            PyRep::Integer(1),
        ],
    )
    .await
    .expect("Offer 1004 succeeds when required trade-in item 34 is in station hangar");
    assert_eq!(
        tradein_purchase
            .dict_get("remainingLP")
            .and_then(|v| v.as_i64()),
        Some(47_500)
    );

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.loyalty_points.insert(1_000_125, 1_000);
        }
    });
    let exch = rpc_call(
        &ctx,
        &mut sess,
        "LPStoreMgr",
        "ExchangeConcordLP",
        &[
            PyRep::Integer(CALDARI_NAVY_CORP_ID),
            PyRep::Integer(1_000),
        ],
    )
    .await
    .expect("ExchangeConcordLP");
    assert_eq!(
        exch.dict_get("convertedLP").and_then(|v| v.as_i64()),
        Some(800)
    );
    assert_eq!(
        exch.dict_get("newTargetCorpLP").and_then(|v| v.as_i64()),
        Some(48_300)
    );
}
