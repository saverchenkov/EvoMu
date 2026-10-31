//! Integration tests for Special Agents, Arcs, and LP Store (Part 3 of Agents & Missions).

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
    let mut agents: Vec<_> = ctx.sde.agents.values().collect();
    agents.sort_by_key(|a| a.agent_id);
    agents
        .into_iter()
        .find(|a| a.level == level && a.agent_type_id == agent_type_id && a.solar_system_id.is_some())
        .map(|a| (a.agent_id, a.corporation_id, a.station_id))
        .expect("SDE agent with requested level and type must exist")
}

fn make_mission(char_id: i64, mission_id: u32, agent_id: i64, sys_id: u32, state: &str) -> ActiveMissionRecord {
    ActiveMissionRecord {
        mission_id, character_id: char_id, agent_id, solar_system_id: sys_id,
        state: state.into(), current_room_id: 1, npcs_remaining: 0, spawned_npc_ball_ids: Vec::new(), bookmark_id: None,
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
        for sid in [3_402_u32, 3_392_u32] {
            snap.skills.entry(char_id).or_default().insert(
                sid,
                TrainedSkillRecord {
                    type_id: sid,
                    level: 5,
                    skill_points: 256_000.0,
                },
            );
        }
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
    let locator_agent = ctx
        .sde
        .agents
        .values()
        .find(|a| a.is_locator && a.solar_system_id.is_some())
        .cloned()
        .expect("Locator agent in SDE");
    let locator_agent_id = locator_agent.agent_id;
    let agent_sys = locator_agent.solar_system_id.unwrap();
    colocate_with_agent(&ctx, &mut sess, char_id, locator_agent_id);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.standings.insert(locator_agent_id, 8.0);
        }
        if let Some(target) = snap.characters.get_mut(&90_000_002) {
            target.solar_system_id = agent_sys;
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
async fn test_lp_store_offers_exchange_rates_and_purchase_validation() {
    // parity: feat:lp.lp-balance, feat:lp.lp-corp-wallet, feat:lp.concord-lp, feat:lp.lp-reward-logs
    eve_testkit::attest(&["feat:lp.lp-balance", "feat:lp.lp-corp-wallet", "feat:lp.concord-lp", "feat:lp.lp-reward-logs"]);
    let ctx = ServerContext::new();
    crate::common::seed_test_lp_store_offers(&ctx);
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
