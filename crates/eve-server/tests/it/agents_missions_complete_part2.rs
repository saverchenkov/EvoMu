//! Integration tests for Mission Lifecycle and Standings Formulas (Part 2 of Agents & Missions).

use crate::common;
use eve_db::{
    ActiveMissionRecord, TrainedSkillRecord, TypeId,
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
        mission_id,
        character_id: char_id,
        agent_id,
        solar_system_id: sys_id,
        state: state.into(),
        current_room_id: 1,
        npcs_remaining: 0,
        spawned_npc_ball_ids: Vec::new(),
        bookmark_id: None,
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

    // R-P0-01 negative regression: insert pilot's personal bookmark in same system & another mission pocket bookmark
    let (personal_bm_id, other_bm_id) = ctx.db.with_mut_snapshot(|snap| {
        let m_sys = snap.active_missions[&(char_id, mission_id_3)].solar_system_id;
        let p_id = 991_001i64;
        let o_id = 991_002i64;
        snap.bookmarks.insert(p_id, eve_db::BookmarkRecord {
            bookmark_id: p_id, owner_id: char_id, solar_system_id: m_sys, label: "Personal Spot".into(),
            x: 100.0, y: 200.0, z: 300.0, item_id: None, memo: "Safe Spot".into(), folder_id: None, is_shared: false,
        });
        snap.bookmarks.insert(o_id, eve_db::BookmarkRecord {
            bookmark_id: o_id, owner_id: char_id, solar_system_id: m_sys, label: "Other Pocket".into(),
            x: 50_000.0, y: 0.0, z: 0.0, item_id: None, memo: "Mission Pocket Destination".into(), folder_id: None, is_shared: false,
        });
        (p_id, o_id)
    });

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
    assert!(ctx.db.with_snapshot(|snap| snap.bookmarks.contains_key(&personal_bm_id)), "personal bookmark in same system must survive mission abandon (R-P0-01)");
    assert!(ctx.db.with_snapshot(|snap| snap.bookmarks.contains_key(&other_bm_id)), "other mission pocket bookmark must survive mission abandon (R-P0-01)");
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
async fn test_standings_formulas_derived_effective_corp_standing_data_and_transactions() {
    // parity: feat:pve.mission-faction-standing-loss, feat:standings.npc-standings, feat:standings.standing-gain-missions, feat:standings.standing-loss, feat:standings.standing-compositions, feat:standings.effective-standing, feat:standings.corp-standings, feat:standings.standing-notifications, feat:standings.trig-edencom-standings
    eve_testkit::attest(&["feat:pve.mission-faction-standing-loss", "feat:standings.npc-standings", "feat:standings.standing-gain-missions", "feat:standings.standing-loss", "feat:standings.standing-compositions", "feat:standings.effective-standing", "feat:standings.corp-standings", "feat:standings.standing-notifications", "feat:standings.trig-edencom-standings"]);
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
    let PyRep::Rowset { columns: npc_cols, rows: npc_rows, .. } = npc_st else {
        panic!("Expected Rowset from GetNPCNPCStandings");
    };
    assert_eq!(npc_cols, vec!["fromID", "toID", "standing"]);
    assert!(!npc_rows.is_empty(), "GetNPCNPCStandings must return derived faction rows");
    let find_npc_rel = |from_id: i64, to_id: i64| -> Option<f64> {
        npc_rows.iter().find_map(|r| {
            if r.first().and_then(|v| v.as_i64()) == Some(from_id) && r.get(1).and_then(|v| v.as_i64()) == Some(to_id) {
                r.get(2).and_then(|v| v.as_f64())
            } else {
                None
            }
        })
    };
    assert_eq!(find_npc_rel(500_001, 500_003), Some(5.0));
    assert_eq!(find_npc_rel(500_001, 500_004), Some(-5.0));
    assert_eq!(find_npc_rel(500_003, 500_002), Some(-5.0));
    assert_eq!(find_npc_rel(500_002, 500_015), Some(4.0));
    assert_eq!(find_npc_rel(500_015, 500_002), Some(3.0));
    assert_eq!(find_npc_rel(500_026, 500_027), Some(-10.0));

    // Verify exact EVE Wiki derived standing formula (NPC_standings.md:140-156):
    // +2% Minmatar (500_002, size 5.0) -> -1% Amarr (500_003, size 5.0, rel -5.0) & +1% Thukker (500_015, size 3.0, rel +3.0)
    use eve_server::services::agents_missions::{apply_faction_standing_with_derived, apply_mission_completion_standings};
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().standings.clear();
        let mut chg = Vec::new();
        apply_faction_standing_with_derived(&ctx, snap, char_id, 500_002, 2.0, true, &mut chg);
        let st = &snap.characters[&char_id].standings;
        assert!((st[&500_002] - 0.20).abs() < 1e-9, "Minmatar +2% gain -> +0.20");
        assert!((st[&500_003] - (-0.10)).abs() < 1e-9, "Amarr -1% derived -> -0.10, got {}", st[&500_003]);
        assert!((st[&500_015] - 0.10).abs() < 1e-9, "Thukker +1% derived -> +0.10, got {}", st[&500_015]);
        assert!((st[&500_001] - (-0.04)).abs() < 1e-9, "Caldari -0.4% derived -> -0.04, got {}", st[&500_001]);

        // Cap at secondary faction's standing toward primary (-5.00 for Amarr -> Minmatar)
        snap.characters.get_mut(&char_id).unwrap().standings.insert(500_003, -4.98);
        apply_faction_standing_with_derived(&ctx, snap, char_id, 500_002, 2.0, true, &mut chg);
        assert!((snap.characters[&char_id].standings[&500_003] - (-5.0)).abs() < 1e-9, "Amarr must cap at -5.00");
        apply_faction_standing_with_derived(&ctx, snap, char_id, 500_002, 2.0, true, &mut chg);
        assert!((snap.characters[&char_id].standings[&500_003] - (-5.0)).abs() < 1e-9, "Amarr at -5.00 must not drop further");

        // Asymmetric standing loss: -2% Minmatar lowers friend Thukker (-1%), never raises enemy Amarr
        snap.characters.get_mut(&char_id).unwrap().standings.clear();
        apply_faction_standing_with_derived(&ctx, snap, char_id, 500_002, -2.0, true, &mut chg);
        let st_loss = &snap.characters[&char_id].standings;
        assert!((st_loss[&500_002] - (-0.20)).abs() < 1e-9);
        assert!((st_loss[&500_015] - (-0.10)).abs() < 1e-9, "Friend Thukker must suffer -1% derived loss");
        assert_eq!(st_loss.get(&500_003).copied().unwrap_or(0.0), 0.0, "Enemy Amarr must NOT gain positive derived standing");

        // Per EVE Wiki (NPC_standings.md:160): intermediate Career Agent missions propagate derived
        // standings, while only the final career mission (e.g. "(10 of 10)") suppresses them.
        if let Some(career_ag) = ctx.sde.agents.values().find(|a| a.agent_type_id == 12 && ctx.sde.npc_corporations.get(&a.corporation_id).and_then(|c| c.faction_id) == Some(500_001)) {
            snap.characters.get_mut(&char_id).unwrap().standings.clear();
            let (_, mid_changes) = apply_mission_completion_standings(&ctx, snap, char_id, career_ag.agent_id, 3096, career_ag.corporation_id, 5.0, false);
            assert!(mid_changes.iter().any(|&(eid, _, delta)| eid == 500_004 && delta < 0.0), "Intermediate career mission (1 of 10) must propagate derived standings");
            snap.characters.get_mut(&char_id).unwrap().standings.clear();
            let (_, fin_changes) = apply_mission_completion_standings(&ctx, snap, char_id, career_ag.agent_id, 3173, career_ag.corporation_id, 5.0, false);
            assert!(!fin_changes.iter().any(|&(eid, _, _)| eid == 500_004), "Final career mission (10 of 10) must suppress derived standings");
        }
        snap.characters.get_mut(&char_id).unwrap().standings.clear();
    });

    // Complete a Storyline mission -> verifies asymptotic gain formula, derived faction propagation, and GetStandingTransactions
    let (sl_agent_id, sl_corp_id, _) = find_agent_by_level_and_type(&ctx, 1, 6);
    let sl_faction_id = ctx.sde.npc_corporations.get(&sl_corp_id).and_then(|c| c.faction_id).map(i64::from).unwrap_or(500_001);
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

    ctx.db.with_snapshot(|snap| {
        let st = &snap.characters[&char_id].standings;
        assert!(st.get(&sl_faction_id).copied().unwrap_or(0.0) > 0.0, "Storyline must grant primary faction standing");
        assert!(st.values().any(|&v| v < 0.0), "Storyline must propagate negative derived standing to hostile factions");
    });

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
    let agents_rep = rpc_call(&ctx, &mut sess, "agentMgr", "GetAgents", &[PyRep::Integer(4)]).await.expect("GetAgents");
    assert!(!matches!(agents_rep, PyRep::None));

    let sys_rep = rpc_call(&ctx, &mut sess, "agentMgr", "GetSolarSystemOfAgent", &[PyRep::Integer(l1_agent_id)]).await.expect("GetSolarSystemOfAgent");
    assert!(sys_rep.as_i64().unwrap_or(0) >= 30_000_000);

    // 2. GetInfoServiceDetails returns authentic SDE attributes
    let details = rpc_call(&ctx, &mut sess, "agentMgr", "GetInfoServiceDetails", &[PyRep::Integer(l4_agent_id)]).await.expect("GetInfoServiceDetails");
    assert_eq!(details.dict_get("agentID").and_then(|v| v.as_i64()), Some(l4_agent_id));
    assert_eq!(details.dict_get("level").and_then(|v| v.as_i64()), Some(4));

    // 3. L1 agent accessible at 0.0 standing, rejected at <= -2.0
    colocate_with_agent(&ctx, &mut sess, char_id, l1_agent_id);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.standings.clear();
        }
    });
    let l1_ok = rpc_call(&ctx, &mut sess, "agentMgr", "DoAction", &[PyRep::Integer(1), PyRep::Integer(l1_agent_id)]).await.expect("L1 agent must allow access at 0.0 standing");
    let l1_mid = l1_ok.dict_get("missionID").and_then(|v| v.as_i64()).unwrap();
    assert!(!l1_ok.dict_get("briefing").and_then(|v| v.as_str()).unwrap_or("").is_empty());
    assert!(!l1_ok.dict_get("objectives").and_then(|v| v.as_str()).unwrap_or("").is_empty());
    assert!(l1_ok.dict_get("iskReward").and_then(|v| v.as_i64()).unwrap_or(0) > 0);
    assert!(l1_ok.dict_get("lpReward").and_then(|v| v.as_i64()).unwrap_or(0) > 0);
    let briefing_info = rpc_call(&ctx, &mut sess, "agentMgr", "GetMissionBriefingInfo", &[PyRep::Integer(l1_mid)]).await.expect("GetMissionBriefingInfo");
    assert_eq!(briefing_info.dict_get("missionID").and_then(|v| v.as_i64()), Some(l1_mid));
    assert!(!briefing_info.dict_get("briefing").and_then(|v| v.as_str()).unwrap_or("").is_empty());

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.standings.insert(l1_corp_id, -2.0);
        }
    });
    let l1_hostile = rpc_call(&ctx, &mut sess, "agentMgr", "DoAction", &[PyRep::Integer(1), PyRep::Integer(l1_agent_id)]).await.expect_err("L1 agent must reject character with <= -2.0 standing");
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
        let below_err = rpc_call(&ctx, &mut sess, "agentMgr", "DoAction", &[PyRep::Integer(1), PyRep::Integer(agent_id)]).await.expect_err("Should reject below level standing threshold");
        assert_eq!(below_err.code, "InsufficientAgentStanding");

        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&char_id) {
                ch.standings.clear();
                ch.standings.insert(agent_id, req_threshold);
            }
        });
        let at_threshold = rpc_call(&ctx, &mut sess, "agentMgr", "DoAction", &[PyRep::Integer(1), PyRep::Integer(agent_id)]).await.expect("MAX(agent, corp, faction) at threshold must succeed");
        assert_eq!(
            at_threshold.dict_get("state").and_then(|v| v.as_str()),
            Some("Offered")
        );
    }
}
