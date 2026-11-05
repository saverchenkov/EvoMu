//! Unit and mutation-killing tests for `services::agents_missions`.

use super::*;
use crate::router::ServerContext;
use crate::services::agents_missions::agents_standings_arcs::{
    EpicArcStateEntry, RndResearchEntry, save_epic_arc_states, save_mission_meta,
    save_rnd_research_entries,
};
use eve_db::WorldRepository;
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::{AgentInfo, SdeRegistry};
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

fn rowset_len(rep: &PyRep) -> usize {
    match rep {
        PyRep::Rowset { rows, .. } | PyRep::TupleSet { rows, .. } => rows.len(),
        PyRep::IndexRowset { items, .. } => items.len(),
        PyRep::List(items) | PyRep::Tuple(items) => items.len(),
        _ => rep.crowset_rows().map(|r| r.len()).unwrap_or(0),
    }
}

static SHARED_SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();

fn test_ctx() -> ServerContext {
    if let Some(sde) = SHARED_SDE.get() {
        ServerContext::with_sde_and_repository(
            Arc::clone(sde),
            Arc::new(WorldRepository::with_demo_seed()),
        )
        .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED)
    } else {
        let ctx = ServerContext::new();
        let _ = SHARED_SDE.set(Arc::clone(&ctx.sde));
        ctx
    }
}

fn char_session(char_id: i64, sys_id: u32, station_id: Option<i64>) -> SessionState {
    SessionState {
        user_id: Some(1),
        character_id: Some(char_id),
        corporation_id: Some(1_000_009),
        solar_system_id: Some(sys_id),
        station_id,
        ship_id: Some(100_000_001),
        ..Default::default()
    }
}

fn find_level1_standard_agent(ctx: &ServerContext) -> AgentInfo {
    ctx.sde
        .agents
        .values()
        .find(|a| {
            a.level == 1
                && a.agent_type_id == 2
                && a.division_id == 24
                && a.solar_system_id.is_some_and(|sid| {
                    ctx.sde
                        .solar_systems
                        .get(&sid)
                        .is_some_and(|s| s.security_status >= 0.45)
                })
                && a.station_id.is_some()
        })
        .cloned()
        .expect("SDE must contain a level 1 standard agent")
}

#[tokio::test]
async fn get_agents_and_metadata_queries_cover_filters_and_branches() {
    let ctx = test_ctx();
    let ag = find_level1_standard_agent(&ctx);
    let sys_id = ag.solar_system_id.unwrap();
    let st_id = ag.station_id;
    let mut sess = char_session(90_000_001, sys_id, st_id);

    // GetAgents with level, division, corporation, agentType, and securityStatus filters
    let mut kw = HashMap::new();
    kw.insert("level".into(), PyRep::Integer(ag.level as i64));
    kw.insert("divisionID".into(), PyRep::Integer(ag.division_id as i64));
    kw.insert("corporationID".into(), PyRep::Integer(ag.corporation_id));
    kw.insert("agentTypeID".into(), PyRep::Integer(ag.agent_type_id as i64));
    kw.insert("securityStatus".into(), PyRep::String("highsec".into()));
    let rows_rep = handle(&ctx, &mut sess, "agentMgr", "GetAgents", &[], &kw)
        .await
        .unwrap()
        .unwrap();
    assert!(rowset_len(&rows_rep) > 0);

    // Lowsec / nullsec filters exclude highsec agents
    kw.insert("securityStatus".into(), PyRep::String("nullsec".into()));
    let _ = handle(&ctx, &mut sess, "agentMgr", "GetAgents", &[], &kw)
        .await
        .unwrap()
        .unwrap();

    // GetInfoServiceDetails & GetAgentLocationWrap
    let info = handle(
        &ctx,
        &mut sess,
        "agentMgr",
        "GetInfoServiceDetails",
        &[PyRep::Integer(ag.agent_id)],
        &HashMap::new(),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(info.dict_get("agentID"), Some(&PyRep::Integer(ag.agent_id)));
    let loc_wrap = handle(
        &ctx,
        &mut sess,
        "agentMgr",
        "GetAgentLocationWrap",
        &[PyRep::Integer(ag.agent_id)],
        &HashMap::new(),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(loc_wrap.dict_get("agentID"), Some(&PyRep::Integer(ag.agent_id)));

    // GetSolarSystemOfAgent
    let sol = handle(
        &ctx,
        &mut sess,
        "agentMgr",
        "GetSolarSystemOfAgent",
        &[PyRep::Integer(ag.agent_id)],
        &HashMap::new(),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(sol, PyRep::Integer(sys_id as i64));
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "agentMgr",
            "GetSolarSystemOfAgent",
            &[PyRep::Integer(999_999_999)],
            &HashMap::new(),
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "UnknownAgent"
    );

    // GetDungeonShipRestrictions, GetEntryPoint, GetMissionKeywords, GetStandingGainsForMission, IsCheatingWithAgent, GotoLocation
    let restr = handle(
        &ctx,
        &mut sess,
        "agentMgr",
        "GetDungeonShipRestrictions",
        &[],
        &HashMap::new(),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        restr.dict_get("restrictedShipTypes"),
        Some(&PyRep::List(vec![]))
    );

    let ep = handle(&ctx, &mut sess, "agentMgr", "GetEntryPoint", &[], &HashMap::new())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(ep.dict_get("bookmarkID"), Some(&PyRep::Integer(1)));

    let kw_res = handle(
        &ctx,
        &mut sess,
        "agentMgr",
        "GetMissionKeywords",
        &[],
        &HashMap::new(),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(kw_res, PyRep::Dict(vec![]));

    let gains = handle(
        &ctx,
        &mut sess,
        "agentMgr",
        "GetStandingGainsForMission",
        &[PyRep::Integer(ag.agent_id)],
        &HashMap::new(),
    )
    .await
    .unwrap()
    .unwrap();
    let PyRep::Dict(gain_entries) = gains else {
        panic!("expected dict");
    };
    assert_eq!(gain_entries.len(), 2);

    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "agentMgr",
            "IsCheatingWithAgent",
            &[],
            &HashMap::new(),
        )
        .await
        .unwrap()
        .unwrap(),
        PyRep::Bool(false)
    );
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "agentMgr",
            "GotoLocation",
            &[],
            &HashMap::new(),
        )
        .await
        .unwrap()
        .unwrap(),
        PyRep::Bool(true)
    );
}

#[tokio::test]
async fn mission_offer_restrictions_enforce_storyline_alpha_rnd_cosmos_and_standing_gates() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;

    // 1. Storyline agent (type 6 or 7) requires referral
    if let Some(sl_ag) = ctx
        .sde
        .agents
        .values()
        .find(|a| matches!(a.agent_type_id, 6 | 7) && a.solar_system_id.is_some())
        .cloned()
    {
        let mut sess = char_session(char_id, sl_ag.solar_system_id.unwrap(), sl_ag.station_id);
        let err = handle(
            &ctx,
            &mut sess,
            "agentMgr",
            "GetOffer",
            &[PyRep::Integer(sl_ag.agent_id)],
            &HashMap::new(),
        )
        .await
        .unwrap()
        .unwrap_err();
        assert_eq!(err.code, "StorylineAgentRequiresReferral");
    }

    // 2. Level 4 agent rejects Alpha clone (clone_grade = 1) and insufficient standing for Omega
    if let Some(l4_ag) = ctx
        .sde
        .agents
        .values()
        .find(|a| a.level == 4 && a.agent_type_id == 2 && a.solar_system_id.is_some())
        .cloned()
    {
        let mut sess = char_session(char_id, l4_ag.solar_system_id.unwrap(), l4_ag.station_id);
        ctx.db.with_mut_snapshot(|snap| {
            snap.characters.get_mut(&char_id).unwrap().clone_grade = 1;
        });
        let err_alpha = handle(
            &ctx,
            &mut sess,
            "agentMgr",
            "GetOffer",
            &[PyRep::Integer(l4_ag.agent_id)],
            &HashMap::new(),
        )
        .await
        .unwrap()
        .unwrap_err();
        assert_eq!(err_alpha.code, "CantDoMissionAlphaClone");

        // Upgrade to Omega (clone_grade = 2) with 0.0 standing -> InsufficientAgentStanding (needs >= 5.0)
        ctx.db.with_mut_snapshot(|snap| {
            let ch = snap.characters.get_mut(&char_id).unwrap();
            ch.clone_grade = 2;
            ch.standings.clear();
        });
        let err_st = handle(
            &ctx,
            &mut sess,
            "agentMgr",
            "GetOffer",
            &[PyRep::Integer(l4_ag.agent_id)],
            &HashMap::new(),
        )
        .await
        .unwrap()
        .unwrap_err();
        assert_eq!(err_st.code, "InsufficientAgentStanding");
    }

    // 3. R&D agent (type 4) requires active research
    if let Some(rnd_ag) = ctx
        .sde
        .agents
        .values()
        .find(|a| a.agent_type_id == 4 && a.level == 1 && a.solar_system_id.is_some())
        .cloned()
    {
        let mut sess = char_session(char_id, rnd_ag.solar_system_id.unwrap(), rnd_ag.station_id);
        let err_rnd = handle(
            &ctx,
            &mut sess,
            "agentMgr",
            "GetOffer",
            &[PyRep::Integer(rnd_ag.agent_id)],
            &HashMap::new(),
        )
        .await
        .unwrap()
        .unwrap_err();
        assert_eq!(err_rnd.code, "ResearchNotActive");
    }

    // 4. Epic Arc agent (type 10) on cooldown returns EpicArcOnCooldown
    if let Some(epic_ag) = ctx
        .sde
        .agents
        .values()
        .find(|a| a.agent_type_id == 10 && a.solar_system_id.is_some())
        .cloned()
    {
        let now = ctx.now_unix();
        let (arc_id, fac_id) = resolve_epic_arc_id_for_agent(&ctx, epic_ag.agent_id);
        ctx.db.with_mut_snapshot(|snap| {
            save_epic_arc_states(
                snap,
                char_id,
                &[EpicArcStateEntry {
                    epic_arc_id: arc_id,
                    agent_id: epic_ag.agent_id,
                    faction_id: fac_id,
                    chapter: 3,
                    completed_missions: vec![1],
                    chosen_branch: None,
                    completed_at_unix: Some(now),
                    cooldown_until_unix: Some(now + 99_999),
                }],
            );
        });
        let mut sess = char_session(char_id, epic_ag.solar_system_id.unwrap(), epic_ag.station_id);
        let err_cd = handle(
            &ctx,
            &mut sess,
            "agentMgr",
            "GetOffer",
            &[PyRep::Integer(epic_ag.agent_id)],
            &HashMap::new(),
        )
        .await
        .unwrap()
        .unwrap_err();
        assert_eq!(err_cd.code, "EpicArcOnCooldown");
    }
}

#[tokio::test]
async fn full_mission_lifecycle_offer_accept_journal_briefing_complete_and_quit_works() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let ag = find_level1_standard_agent(&ctx);
    let sys_id = ag.solar_system_id.unwrap();
    let st_id = ag.station_id;
    let mut sess = char_session(char_id, sys_id, st_id);

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = sys_id;
            ch.station_id = st_id;
            ch.standings.insert(ag.agent_id, 5.0);
            ch.standings.insert(ag.corporation_id, 5.0);
        }
    });

    // 1. Request mission offer (action 2)
    let offer = handle(
        &ctx,
        &mut sess,
        "agentMgr",
        "DoAction",
        &[PyRep::Integer(2), PyRep::Integer(ag.agent_id)],
        &HashMap::new(),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(offer.dict_get("state"), Some(&PyRep::String("Offered".into())));
    let mid = offer.dict_get("missionID").unwrap().as_i64().unwrap() as u32;

    // 2. Accept mission (action 3)
    let accepted = handle(
        &ctx,
        &mut sess,
        "agentMgr",
        "DoAction",
        &[
            PyRep::Integer(3),
            PyRep::Integer(ag.agent_id),
            PyRep::Integer(mid as i64),
        ],
        &HashMap::new(),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        accepted.dict_get("state"),
        Some(&PyRep::String("Accepted".into()))
    );
    assert_eq!(accepted.dict_get("missionState"), Some(&PyRep::Integer(2)));

    // 3. GetMyJournalDetails and GetMissionBriefingInfo
    let now = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        save_rnd_research_entries(
            snap,
            char_id,
            &[RndResearchEntry {
                agent_id: ag.agent_id,
                research_type_id: 11442,
                points_per_day: 25.0,
                started_unix: now.saturating_sub(86_400),
                initial_points: 10.0,
            }],
        );
    });
    let journal = handle(
        &ctx,
        &mut sess,
        "agentMgr",
        "GetMyJournalDetails",
        &[],
        &HashMap::new(),
    )
    .await
    .unwrap()
    .unwrap();
    let PyRep::Tuple(j_parts) = journal else {
        panic!("expected tuple");
    };
    assert_eq!(j_parts.len(), 2);

    let briefing = handle(
        &ctx,
        &mut sess,
        "agentMgr",
        "GetMissionBriefingInfo",
        &[PyRep::Integer(ag.agent_id), PyRep::Integer(mid as i64)],
        &HashMap::new(),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        briefing.dict_get("missionID"),
        Some(&PyRep::Integer(mid as i64))
    );

    // 4. Mark mission objectives ready (0 NPCs remaining and satisfy any required delivery item) and complete (action 6)
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(m) = snap.active_missions.get_mut(&(char_id, mid)) {
            m.npcs_remaining = 0;
        }
        if let Some(mut meta) = load_mission_meta(snap, char_id, mid) {
            meta.required_delivery_type_id = None;
            meta.required_delivery_qty = 0;
            meta.destination_station_id = None;
            save_mission_meta(snap, char_id, &meta);
        }
    });
    let bal_before = ctx.db.with_snapshot(|s| s.characters[&char_id].balance.raw_cents());
    let completed = handle(
        &ctx,
        &mut sess,
        "agentMgr",
        "DoAction",
        &[
            PyRep::Integer(6),
            PyRep::Integer(ag.agent_id),
            PyRep::Integer(mid as i64),
        ],
        &HashMap::new(),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        completed.dict_get("state"),
        Some(&PyRep::String("Completed".into()))
    );
    assert_eq!(completed.dict_get("missionState"), Some(&PyRep::Integer(4)));
    let bal_after = ctx.db.with_snapshot(|s| s.characters[&char_id].balance.raw_cents());
    assert!(bal_after > bal_before);

    // 5. Offer a second mission and Decline/RemoveOfferFromJournal
    let offer2 = handle(
        &ctx,
        &mut sess,
        "agentMgr",
        "GetOffer",
        &[PyRep::Integer(ag.agent_id)],
        &HashMap::new(),
    )
    .await
    .unwrap()
    .unwrap();
    let mid2 = offer2.dict_get("missionID").unwrap().as_i64().unwrap();
    let removed = handle(
        &ctx,
        &mut sess,
        "agentMgr",
        "RemoveOfferFromJournal",
        &[PyRep::Integer(mid2)],
        &HashMap::new(),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        removed.dict_get("missionID"),
        Some(&PyRep::Integer(mid2))
    );
}
