//! Semantic unit tests split from `agents_missions_extra_unit_tests`.

#![forbid(unsafe_code)]

use super::*;
use crate::services::agents_missions::agents_standings_arcs::{
    RndResearchEntry, save_mission_meta, save_rnd_research_entries,
};
use eve_db::{BookmarkRecord, InventoryFlag, InventoryItemRecord};
use eve_proto::PyRep;
use std::collections::HashMap;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn journal_expiration_sweep_and_briefing_objective_formatting_branches() {
    let ctx = test_ctx();
    ctx.set_clock(std::sync::Arc::new(
        eve_db::clock::VirtualServerClock::with_fixed_base(1_800_000_000),
    ));
    let char_id = 90_000_001_i64;
    let ag = find_standard_agent(&ctx, 1);
    let sys_id = ag.solar_system_id.unwrap();
    let st_id = ag.station_id.unwrap();
    let mut sess = char_session(char_id, Some(sys_id), Some(st_id));
    let kw = HashMap::new();
    let now = ctx.now_unix();

    let (mid_exp_offer, mid_exp_accept, mid_active) = (10_001_u32, 10_002_u32, 10_003_u32);

    ctx.db.with_mut_snapshot(|snap| {
        snap.active_missions.insert(
            (char_id, mid_exp_offer),
            make_active_mission(mid_exp_offer, char_id, ag.agent_id, sys_id, "Offered", None),
        );
        save_mission_meta(snap, char_id, &make_meta(mid_exp_offer, ag.agent_id, now.saturating_sub(10), None));

        snap.active_missions.insert(
            (char_id, mid_exp_accept),
            make_active_mission(mid_exp_accept, char_id, ag.agent_id, sys_id, "Accepted", None),
        );
        save_mission_meta(
            snap,
            char_id,
            &make_meta(mid_exp_accept, ag.agent_id, now + 1000, Some(now.saturating_sub(5))),
        );

        snap.active_missions.insert(
            (char_id, mid_active),
            make_active_mission(mid_active, char_id, ag.agent_id, sys_id, "Accepted", Some(888_001)),
        );
        snap.bookmarks.insert(
            888_001,
            BookmarkRecord {
                bookmark_id: 888_001,
                owner_id: char_id,
                solar_system_id: sys_id,
                label: "Pocket BM".into(),
                x: 1000.0,
                y: 2000.0,
                z: 3000.0,
                item_id: None,
                memo: "Mission Pocket Destination".into(),
                folder_id: None,
                is_shared: false,
            },
        );
        let mut active_meta = make_meta(mid_active, ag.agent_id, now + 86_400, Some(now + 86_400));
        active_meta.required_delivery_type_id = Some(34);
        active_meta.required_delivery_qty = 250;
        active_meta.destination_station_id = Some(st_id);
        active_meta.origin_station_id = Some(st_id);
        active_meta.offered_level = Some(3);
        save_mission_meta(snap, char_id, &active_meta);

        save_rnd_research_entries(
            snap,
            char_id,
            &[RndResearchEntry {
                agent_id: ag.agent_id,
                research_type_id: 11442,
                points_per_day: 50.0,
                started_unix: now.saturating_sub(2 * 86_400),
                initial_points: 100.0,
            }],
        );
    });

    let journal = call_agent_mgr(&ctx, &mut sess, "GetMyJournalDetails", &[], &kw).await.unwrap();
    let PyRep::Tuple(parts) = journal else { panic!("expected tuple") };
    let PyRep::List(missions) = &parts[0] else { panic!("expected missions list") };
    assert_eq!(missions.len(), 1);
    let PyRep::Tuple(m0) = &missions[0] else { panic!("expected V24 10-tuple mission row") };
    assert_eq!(m0.len(), 10);
    assert_eq!(m0[0], PyRep::Integer(2));
    assert_eq!(m0[2], PyRep::String("UI/Agents/MissionTypes/Courier".into()));
    assert_eq!(m0[9], PyRep::Integer(mid_active as i64));

    let PyRep::List(research) = &parts[1] else { panic!("expected research list") };
    assert_eq!(research[0].dict_get("points"), Some(&PyRep::Real(200.0)));

    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.active_missions[&(char_id, mid_exp_offer)].state, "Expired");
        assert_eq!(snap.active_missions[&(char_id, mid_exp_accept)].state, "Failed");
        assert_eq!(snap.active_missions[&(char_id, mid_exp_accept)].npcs_remaining, 0);
    });

    let obj_info = call_agent_mgr(
        &ctx,
        &mut sess,
        "GetMissionObjectiveInfo",
        &[PyRep::Integer(ag.agent_id), PyRep::Integer(mid_active as i64)],
        &kw,
    )
    .await
    .unwrap();
    let Some(PyRep::List(objs)) = obj_info.dict_get("objectives") else { panic!("expected V24 objectives list") };
    assert_eq!(objs.len(), 2);
    let PyRep::Tuple(trans_entry) = &objs[1] else { panic!("expected transport tuple") };
    assert_eq!(trans_entry[0], PyRep::String("transport".into()));
    let PyRep::Tuple(trans_args) = &trans_entry[1] else { panic!("expected transport args") };
    assert_eq!(trans_args[4].dict_get("typeID"), Some(&PyRep::Integer(34)));
    assert_eq!(trans_args[4].dict_get("quantity"), Some(&PyRep::Integer(250)));

    let failed_info = call_agent_mgr(
        &ctx,
        &mut sess,
        "GetMissionJournalInfo",
        &[PyRep::Integer(ag.agent_id), PyRep::Integer(mid_exp_accept as i64)],
        &kw,
    )
    .await
    .unwrap();
    assert_eq!(failed_info.dict_get("missionState"), Some(&PyRep::Integer(3)));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn do_action_colocation_fw_cosmos_and_standing_error_branches() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let ag = find_standard_agent(&ctx, 1);
    let sys_id = ag.solar_system_id.unwrap();
    let st_id = ag.station_id.unwrap();
    let kw = HashMap::new();

    // 1. AgentNotCoLocated when character is in a different solar system
    let mut wrong_sys_sess = char_session(char_id, Some(sys_id + 1), Some(st_id));
    assert_eq!(
        call_agent_mgr(&ctx, &mut wrong_sys_sess, "DoAction", &[PyRep::Integer(2), PyRep::Integer(ag.agent_id)], &kw)
            .await
            .unwrap_err()
            .code,
        "AgentNotCoLocated"
    );

    // 2. Level 1 standard agent with standing <= -2.0 -> InsufficientAgentStanding
    let mut sess = char_session(char_id, Some(sys_id), Some(st_id));
    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&char_id).unwrap();
        ch.standings.clear();
        ch.standings.insert(ag.agent_id, -2.0);
        ch.standings.insert(ag.corporation_id, -2.0);
    });
    assert_eq!(
        call_agent_mgr(&ctx, &mut sess, "DoAction", &[PyRep::Integer(2), PyRep::Integer(ag.agent_id)], &kw)
            .await
            .unwrap_err()
            .code,
        "InsufficientAgentStanding"
    );

    // 3. FW agent (agent_type_id == 9) requires militia enlistment
    if let Some(fw_ag) = ctx
        .sde
        .agents
        .values()
        .filter(|a| {
            a.agent_type_id == 9
                && a.solar_system_id.is_some()
                && ctx.sde.factions.values().any(|f| f.militia_corporation_id == Some(a.corporation_id))
        })
        .min_by_key(|a| a.agent_id)
        .cloned()
    {
        let mut fw_sess = char_session(char_id, fw_ag.solar_system_id, fw_ag.station_id);
        ctx.db.with_mut_snapshot(|snap| {
            snap.characters.get_mut(&char_id).unwrap().war_faction_id = None;
        });
        assert_eq!(
            call_agent_mgr(&ctx, &mut fw_sess, "GetOffer", &[PyRep::Integer(fw_ag.agent_id)], &kw)
                .await
                .unwrap_err()
                .code,
            "NotEnlistedInMilitia"
        );
    }

    // 4. One-time COSMOS / Career agent (type 5, 8, or 12) Completed vs Expired checks
    if let Some(cosmos_ag) = ctx
        .sde
        .agents
        .values()
        .filter(|a| matches!(a.agent_type_id, 5 | 8 | 12) && a.solar_system_id.is_some())
        .min_by_key(|a| a.agent_id)
        .cloned()
    {
        let mut c_sess = char_session(char_id, cosmos_ag.solar_system_id, cosmos_ag.station_id);
        ctx.db.with_mut_snapshot(|snap| {
            snap.active_missions.insert(
                (char_id, 88_001),
                make_active_mission(88_001, char_id, cosmos_ag.agent_id, cosmos_ag.solar_system_id.unwrap(), "Completed", None),
            );
        });
        assert_eq!(
            call_agent_mgr(&ctx, &mut c_sess, "GetOffer", &[PyRep::Integer(cosmos_ag.agent_id)], &kw)
                .await
                .unwrap_err()
                .code,
            "OneTimeAgentMissionCompleted"
        );

        ctx.db.with_mut_snapshot(|snap| {
            snap.active_missions.get_mut(&(char_id, 88_001)).unwrap().state = "Expired".into();
        });
        assert_eq!(
            call_agent_mgr(&ctx, &mut c_sess, "GetOffer", &[PyRep::Integer(cosmos_ag.agent_id)], &kw)
                .await
                .unwrap_err()
                .code,
            "CosmosMissionExpired"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mission_completion_expiry_penalty_and_courier_delivery_branches() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let ag = find_standard_agent(&ctx, 1);
    let sys_id = ag.solar_system_id.unwrap();
    let st_id = ag.station_id.unwrap();
    let mut sess = char_session(char_id, Some(sys_id), Some(st_id));
    let kw = HashMap::new();
    let now = ctx.now_unix();
    let mid = ctx
        .sde
        .missions
        .iter()
        .filter(|(_, t)| {
            let ct = t.content_template.as_deref().unwrap_or_default();
            t.is_courier && !ct.contains("Trade") && !ct.contains("Heraldry")
        })
        .min_by_key(|&(&id, _)| id)
        .or_else(|| {
            ctx.sde
                .missions
                .iter()
                .filter(|(_, t)| {
                    let ct = t.content_template.as_deref().unwrap_or_default();
                    !ct.contains("Trade") && !ct.contains("Heraldry")
                })
                .min_by_key(|&(&id, _)| id)
        })
        .map(|(&id, _)| id)
        .unwrap_or_else(|| ctx.sde.missions.keys().copied().min().expect("SDE mission"));

    // 1. Accepting an expired offer returns MissionOfferExpired
    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&char_id).unwrap();
        ch.clone_grade = 2;
        ch.solar_system_id = sys_id;
        ch.station_id = Some(st_id);
        ch.standings.insert(ag.agent_id, 5.0);
        ch.standings.insert(ag.corporation_id, 5.0);
        snap.active_missions.insert(
            (char_id, mid),
            make_active_mission(mid, char_id, ag.agent_id, sys_id, "Offered", None),
        );
        save_mission_meta(snap, char_id, &make_meta(mid, ag.agent_id, now.saturating_sub(1), None));
    });
    assert_eq!(
        call_agent_mgr(
            &ctx,
            &mut sess,
            "DoAction",
            &[PyRep::Integer(3), PyRep::Integer(ag.agent_id), PyRep::Integer(mid as i64)],
            &kw,
        )
        .await
        .unwrap_err()
        .code,
        "MissionOfferExpired"
    );

    // 2. Completing an expired accepted mission returns MissionExpired and deducts standings
    ctx.db.with_mut_snapshot(|snap| {
        let m_mut = snap.active_missions.get_mut(&(char_id, mid)).unwrap();
        m_mut.state = "Accepted".into();
        m_mut.npcs_remaining = 0;
        save_mission_meta(
            snap,
            char_id,
            &make_meta(mid, ag.agent_id, now + 500, Some(now.saturating_sub(1))),
        );
    });
    assert_eq!(
        call_agent_mgr(
            &ctx,
            &mut sess,
            "DoAction",
            &[PyRep::Integer(6), PyRep::Integer(ag.agent_id), PyRep::Integer(mid as i64)],
            &kw,
        )
        .await
        .unwrap_err()
        .code,
        "MissionExpired"
    );
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.active_missions[&(char_id, mid)].state, "Failed");
        assert!((snap.characters[&char_id].standings[&ag.agent_id] - 4.90).abs() < 1e-6);
    });

    // 3. Courier mission completion: wrong station -> CourierNotAtDestinationStation, missing cargo -> MissionIncomplete, valid cargo -> Completed
    let dest_st = ctx
        .sde
        .stations
        .keys()
        .copied()
        .filter(|&k| k != st_id)
        .min()
        .unwrap_or(st_id + 1);
    ctx.db.with_mut_snapshot(|snap| {
        let m_mut = snap.active_missions.get_mut(&(char_id, mid)).unwrap();
        m_mut.state = "Accepted".into();
        m_mut.npcs_remaining = 0;
        let mut cmeta = make_meta(mid, ag.agent_id, now + 86_400, Some(now + 86_400));
        cmeta.bonus_expiry_unix = Some(now.saturating_sub(10)); // expired bonus -> bonusReward == 0
        cmeta.required_delivery_type_id = Some(3683);
        cmeta.required_delivery_qty = 10;
        cmeta.granted_courier_item_id = Some(950_001);
        cmeta.granted_courier_qty = 10;
        cmeta.destination_station_id = Some(dest_st);
        cmeta.origin_station_id = Some(st_id);
        save_mission_meta(snap, char_id, &cmeta);
    });
    let expired_obj = call_agent_mgr(
        &ctx,
        &mut sess,
        "GetMissionObjectiveInfo",
        &[PyRep::Integer(ag.agent_id), PyRep::Integer(mid as i64)],
        &kw,
    )
    .await
    .expect("GetMissionObjectiveInfo when bonus expired");
    assert_eq!(
        expired_obj.dict_get("bonusRewards"),
        Some(&PyRep::List(Vec::new())),
        "GetMissionObjectiveInfo must also clear bonusRewards once bonusExpiryUnix passes"
    );
    assert_eq!(
        call_agent_mgr(
            &ctx,
            &mut sess,
            "DoAction",
            &[PyRep::Integer(6), PyRep::Integer(ag.agent_id), PyRep::Integer(mid as i64)],
            &kw,
        )
        .await
        .unwrap_err()
        .code,
        "CourierNotAtDestinationStation"
    );

    sess.station_id = Some(dest_st);
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().station_id = Some(dest_st);
    });
    assert_eq!(
        call_agent_mgr(
            &ctx,
            &mut sess,
            "DoAction",
            &[PyRep::Integer(6), PyRep::Integer(ag.agent_id), PyRep::Integer(mid as i64)],
            &kw,
        )
        .await
        .unwrap_err()
        .code,
        "MissionIncomplete"
    );

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            950_001,
            InventoryItemRecord {
                item_id: 950_001,
                type_id: 3683,
                owner_id: char_id,
                location_id: dest_st,
                flag: InventoryFlag::Hangar,
                quantity: 10,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        snap.item_custom_names.insert(950_001, format!("courier_cargo:{char_id}:{mid}"));
    });
    let done = call_agent_mgr(
        &ctx,
        &mut sess,
        "DoAction",
        &[PyRep::Integer(6), PyRep::Integer(ag.agent_id), PyRep::Integer(mid as i64)],
        &kw,
    )
    .await
    .unwrap();
    assert_eq!(done.dict_get("state"), Some(&PyRep::String("Completed".into())));
    assert_eq!(done.dict_get("bonusReward"), Some(&PyRep::Integer(0)));
    ctx.db.with_snapshot(|snap| assert!(!snap.items.contains_key(&950_001)));

    // 4. Issue #979: Remote Start Conversation (DoAction(None)) returns KeyVal dialogue
    //    both when no mission is accepted ("NotCoLocated") and when an "Accepted" mission is active,
    //    while explicit DoAction(1) / GetOffer from a remote system still returns AgentNotCoLocated.
    let remote_sys_id = ctx
        .sde
        .solar_systems
        .keys()
        .copied()
        .find(|&s| s != sys_id)
        .unwrap_or(30_000_142);
    let mut remote_sess = char_session(char_id, Some(remote_sys_id), None);
    let mut kw_agent = HashMap::new();
    kw_agent.insert("agentID".into(), PyRep::Integer(ag.agent_id));
    let remote_greet = call_agent_mgr(&ctx, &mut remote_sess, "DoAction", &[PyRep::None], &kw_agent)
        .await
        .expect("remote DoAction(None) greeting should succeed");
    assert_eq!(
        remote_greet.dict_get("state"),
        Some(&PyRep::String("NotCoLocated".into()))
    );
    assert_eq!(
        remote_greet.dict_get("dialogue"),
        Some(&PyRep::List(Vec::new()))
    );
    let Some(PyRep::String(agent_says)) = remote_greet.dict_get("agentSays") else {
        panic!("remote greeting must include agentSays string");
    };
    assert!(
        !agent_says.contains("Some(") && !agent_says.contains("None"),
        "remote greeting dialogue must not format Rust Option Debug output: {agent_says}"
    );
    assert_eq!(
        remote_greet.dict_get("briefing"),
        Some(&PyRep::String(agent_says.clone()))
    );
    assert_eq!(
        call_agent_mgr(
            &ctx,
            &mut remote_sess,
            "DoAction",
            &[PyRep::Integer(1), PyRep::Integer(ag.agent_id)],
            &kw,
        )
        .await
        .unwrap_err()
        .code,
        "AgentNotCoLocated"
    );

    ctx.db.with_mut_snapshot(|snap| {
        snap.active_missions.insert(
            (char_id, mid),
            make_active_mission(mid, char_id, ag.agent_id, sys_id, "Accepted", None),
        );
    });
    let remote_accepted_greet =
        call_agent_mgr(&ctx, &mut remote_sess, "DoAction", &[PyRep::None], &kw_agent)
            .await
            .expect("remote DoAction(None) with Accepted mission should return mission briefing");
    assert_eq!(
        remote_accepted_greet.dict_get("state"),
        Some(&PyRep::String("Accepted".into()))
    );
    assert_eq!(
        remote_accepted_greet.dict_get("missionState"),
        Some(&PyRep::Integer(2))
    );

    // 5. Issues #791 & #979: Calling GetMissionBriefingInfo / GetMissionObjectiveInfo on a different
    //    agent when an active mission exists for `ag.agent_id` returns MissionNotActive instead of
    //    leaking `ag.agent_id`'s mission into another agent's dialogue window.
    let other_agent_id = ctx
        .sde
        .agents
        .keys()
        .copied()
        .find(|&aid| aid != ag.agent_id)
        .unwrap_or(ag.agent_id + 1);
    assert_eq!(
        call_agent_mgr(
            &ctx,
            &mut remote_sess,
            "GetMissionBriefingInfo",
            &[PyRep::Integer(other_agent_id)],
            &kw,
        )
        .await
        .unwrap_err()
        .code,
        "MissionNotActive"
    );
    assert_eq!(
        call_agent_mgr(
            &ctx,
            &mut remote_sess,
            "GetMissionObjectiveInfo",
            &[PyRep::Integer(other_agent_id)],
            &kw,
        )
        .await
        .expect("unbound GetMissionObjectiveInfo returns None when not active"),
        PyRep::None
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unbound_briefing_and_objective_info_reject_completed_and_declined_missions() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let ag = find_standard_agent(&ctx, 1);
    let sys_id = ag.solar_system_id.unwrap();
    let st_id = ag.station_id.unwrap();
    let mut sess = char_session(char_id, Some(sys_id), Some(st_id));
    let kw = HashMap::new();

    for (state, mid) in [("Completed", 10_201_u32), ("Declined", 10_202_u32)] {
        ctx.db.with_mut_snapshot(|snap| {
            snap.active_missions.clear();
            snap.active_missions.insert(
                (char_id, mid),
                make_active_mission(mid, char_id, ag.agent_id, sys_id, state, None),
            );
        });
        assert_eq!(
            call_agent_mgr(
                &ctx,
                &mut sess,
                "GetMissionBriefingInfo",
                &[PyRep::Integer(ag.agent_id)],
                &kw,
            )
            .await
            .unwrap_err()
            .code,
            "MissionNotActive",
            "unbound GetMissionBriefingInfo(agent_id) must reject {state} missions"
        );
        assert_eq!(
            call_agent_mgr(
                &ctx,
                &mut sess,
                "GetMissionObjectiveInfo",
                &[PyRep::Integer(ag.agent_id)],
                &kw,
            )
            .await
            .expect("unbound GetMissionObjectiveInfo returns None for completed/declined"),
            PyRep::None,
            "unbound GetMissionObjectiveInfo(agent_id) must return None for {state} missions"
        );
        assert_eq!(
            call_agent_mgr(
                &ctx,
                &mut sess,
                "GetMissionBriefingInfo",
                &[],
                &kw,
            )
            .await
            .unwrap_err()
            .code,
            "MissionNotActive",
            "unbound GetMissionBriefingInfo() with no args must reject {state} missions"
        );
    }

    // Regression check: explicit (agent_id, mission_id) still works for completed/declined history lookup
    let ok_obj = call_agent_mgr(
        &ctx,
        &mut sess,
        "GetMissionObjectiveInfo",
        &[PyRep::Integer(ag.agent_id), PyRep::Integer(10_202)],
        &kw,
    )
    .await
    .expect("explicit mission_id lookup remains valid");
    assert!(ok_obj.dict_get("objectives").is_some());

    // Unbound Offered mission remains accessible when co-located and blocked when remote
    ctx.db.with_mut_snapshot(|snap| {
        snap.active_missions.insert(
            (char_id, 10_203),
            make_active_mission(10_203, char_id, ag.agent_id, sys_id, "Offered", None),
        );
    });
    assert!(call_agent_mgr(
        &ctx,
        &mut sess,
        "GetMissionBriefingInfo",
        &[PyRep::Integer(ag.agent_id)],
        &kw,
    )
    .await
    .is_ok());

    let mut remote_sess = char_session(char_id, Some(sys_id + 1), None);
    assert_eq!(
        call_agent_mgr(
            &ctx,
            &mut remote_sess,
            "GetMissionBriefingInfo",
            &[PyRep::Integer(ag.agent_id)],
            &kw,
        )
        .await
        .unwrap_err()
        .code,
        "MissionNotActive"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn gh998_gh999_gh1000_bookmarks_dungeon_owner_and_courier_item_notification() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let ag = find_standard_agent(&ctx, 1);
    let sys_id = ag.solar_system_id.unwrap();
    let st_id = ag.station_id.unwrap();
    let mut sess = char_session(char_id, Some(sys_id), Some(st_id));
    let kw = HashMap::new();
    let now = ctx.now_unix();

    // Pick a real non-courier mission (#998 bookmark + #999 dungeon ownerID) and a deterministic non-Trade/non-Heraldry courier mission (#1000, #1370).
    let encounter_mid = ctx
        .sde
        .missions
        .values()
        .filter(|m| {
            let ct = m.content_template.as_deref().unwrap_or_default();
            !m.is_courier
                && !ct.contains("Courier")
                && !ct.contains("Trade")
                && !ct.contains("Heraldry")
        })
        .min_by_key(|m| m.mission_id)
        .map(|m| m.mission_id)
        .unwrap_or(10_301);
    let courier_mid = ctx
        .sde
        .missions
        .values()
        .filter(|m| {
            let ct = m.content_template.as_deref().unwrap_or_default();
            m.is_courier && !ct.contains("Trade") && !ct.contains("Heraldry")
        })
        .min_by_key(|m| m.mission_id)
        .map(|m| m.mission_id)
        .expect("SDE should contain at least one courier mission");

    ctx.db.with_mut_snapshot(|snap| {
        snap.active_missions.insert(
            (char_id, encounter_mid),
            make_active_mission(encounter_mid, char_id, ag.agent_id, sys_id, "Accepted", Some(888_998)),
        );
        save_mission_meta(
            snap,
            char_id,
            &make_meta(encounter_mid, ag.agent_id, now + 86_400, Some(now + 86_400)),
        );
        snap.bookmarks.insert(
            888_998,
            BookmarkRecord {
                bookmark_id: 888_998,
                owner_id: char_id,
                solar_system_id: sys_id,
                label: "Mission Pocket".into(),
                x: 100.0,
                y: 200.0,
                z: 300.0,
                item_id: None,
                memo: "Mission Pocket Destination".into(),
                folder_id: None,
                is_shared: false,
            },
        );
    });

    // #998: GetMyJournalDetails bookmarks include hint, locationID, and 0-based locationNumber
    let journal = call_agent_mgr(&ctx, &mut sess, "GetMyJournalDetails", &[], &kw)
        .await
        .expect("GetMyJournalDetails");
    let PyRep::Tuple(j_parts) = journal else {
        panic!("expected tuple from GetMyJournalDetails");
    };
    let PyRep::List(ref j_missions) = j_parts[0] else {
        panic!("expected missions list");
    };
    let PyRep::Tuple(enc_row) = j_missions
        .iter()
        .find(|r| {
            matches!(r, PyRep::Tuple(cols) if cols.get(9) == Some(&PyRep::Integer(encounter_mid as i64)))
        })
        .expect("encounter mission in journal")
    else {
        panic!("expected 10-tuple journal row");
    };
    let PyRep::List(bms) = &enc_row[6] else {
        panic!("expected bookmarks list");
    };
    assert_eq!(bms.len(), 1);
    assert!(
        bms[0]
            .dict_get("hint")
            .and_then(PyRep::as_str)
            .is_some_and(|s| s.starts_with("UI/Agents/"))
    );
    assert_eq!(bms[0].dict_get("locationID"), Some(&PyRep::Integer(sys_id as i64)));
    assert_eq!(bms[0].dict_get("solarSystemID"), Some(&PyRep::Integer(sys_id as i64)));
    assert_eq!(bms[0].dict_get("solarsystemID"), Some(&PyRep::Integer(sys_id as i64)));
    assert_eq!(bms[0].dict_get("locationNumber"), Some(&PyRep::Integer(0)));
    assert_eq!(bms[0].dict_get("agentID"), Some(&PyRep::Integer(ag.agent_id)));
    assert_eq!(bms[0].dict_get("locationType"), Some(&PyRep::String("dungeon".into())));
    assert_eq!(bms[0].dict_get("itemID"), Some(&PyRep::None));
    assert_eq!(bms[0].dict_get("typeID"), Some(&PyRep::Integer(5)));

    // #1017: Bookmark referencing an SDE station item carries itemID and station_type_id.
    let station_id = ctx
        .sde
        .stations
        .values()
        .find(|s| s.solar_system_id == sys_id)
        .map(|s| (s.station_id, s.station_type_id as i64));
    if let Some((st_id, st_type_id)) = station_id {
        let st_bm_id = 91_000_002_i64;
        ctx.db.with_mut_snapshot(|snap| {
            snap.bookmarks.insert(
                st_bm_id,
                eve_db::BookmarkRecord {
                    bookmark_id: st_bm_id,
                    owner_id: char_id,
                    item_id: Some(st_id),
                    solar_system_id: sys_id,
                    x: 100.0,
                    y: 200.0,
                    z: 300.0,
                    label: "Mission Pocket Station".into(),
                    memo: "Mission Pocket Station".into(),
                    folder_id: None,
                    is_shared: false,
                },
            );
        });
        let journal2 = call_agent_mgr(&ctx, &mut sess, "GetMyJournalDetails", &[], &kw)
            .await
            .expect("GetMyJournalDetails with station bookmark");
        let PyRep::Tuple(j2_parts) = journal2 else {
            panic!("expected tuple");
        };
        let PyRep::List(ref j2_missions) = j2_parts[0] else {
            panic!("expected missions");
        };
        let PyRep::Tuple(enc_row2) = j2_missions
            .iter()
            .find(|r| {
                matches!(r, PyRep::Tuple(cols) if cols.get(9) == Some(&PyRep::Integer(encounter_mid as i64)))
            })
            .expect("encounter row2")
        else {
            panic!("expected 10-tuple journal row2");
        };
        let PyRep::List(bms2) = &enc_row2[6] else {
            panic!("expected bookmarks list2");
        };
        let st_bm = bms2
            .iter()
            .find(|b| b.dict_get("bookmarkID") == Some(&PyRep::Integer(st_bm_id)))
            .expect("station bookmark");
        assert_eq!(st_bm.dict_get("itemID"), Some(&PyRep::Integer(st_id)));
        assert_eq!(st_bm.dict_get("typeID"), Some(&PyRep::Integer(st_type_id)));
        assert_eq!(st_bm.dict_get("solarsystemID"), Some(&PyRep::Integer(sys_id as i64)));
    }

    // #999: GetMissionObjectiveInfo dungeons[].ownerID is an NPC faction or corp ID, never ag.agent_id
    let obj_info = call_agent_mgr(
        &ctx,
        &mut sess,
        "GetMissionObjectiveInfo",
        &[PyRep::Integer(ag.agent_id), PyRep::Integer(encounter_mid as i64)],
        &kw,
    )
    .await
    .expect("GetMissionObjectiveInfo");
    let Some(PyRep::List(dungeons)) = obj_info.dict_get("dungeons") else {
        panic!("expected dungeons list");
    };
    assert_eq!(dungeons.len(), 1);
    let dun_owner = dungeons[0]
        .dict_get("ownerID")
        .and_then(PyRep::as_i64)
        .expect("dungeon ownerID");
    assert_ne!(dun_owner, ag.agent_id, "dungeon ownerID must never be agent character ID");
    assert!(
        (500_000..3_000_000).contains(&dun_owner),
        "dungeon ownerID {dun_owner} must be NPC faction or corp ID"
    );

    // #1000: Accepting a Courier mission via DoAction(3) emits OnItemChange for granted_courier_item_id
    ctx.db.with_mut_snapshot(|snap| {
        snap.active_missions.insert(
            (char_id, courier_mid),
            make_active_mission(courier_mid, char_id, ag.agent_id, sys_id, "Offered", None),
        );
        let mut cmeta = make_meta(courier_mid, ag.agent_id, now + 86_400, None);
        cmeta.required_delivery_type_id = Some(34);
        cmeta.required_delivery_qty = 6;
        cmeta.origin_station_id = Some(st_id);
        cmeta.destination_station_id = Some(st_id);
        save_mission_meta(snap, char_id, &cmeta);
    });
    let _ = ctx.db.acquire_online_lock(1, sess.session_id, Some(char_id), false, now);
    let _ = ctx.drain_notifications_for_char(char_id);

    call_agent_mgr(
        &ctx,
        &mut sess,
        "DoAction",
        &[
            PyRep::Integer(3),
            PyRep::Integer(ag.agent_id),
            PyRep::Integer(courier_mid as i64),
        ],
        &kw,
    )
    .await
    .expect("DoAction(3) accept courier");

    let granted_cid = ctx
        .db
        .with_snapshot(|snap| load_mission_meta(snap, char_id, courier_mid).and_then(|m| m.granted_courier_item_id))
        .expect("granted_courier_item_id recorded");
    let notifs = ctx.drain_notifications_for_char(char_id);
    assert!(
        notifs.iter().any(|n| {
            n.event == "OnItemChange"
                && format!("{:?}", n.args).contains(&granted_cid.to_string())
        }),
        "Accepting Courier mission must emit OnItemChange containing granted_courier_item_id={granted_cid}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bound_get_my_journal_details_filters_by_bound_agent_id_1016() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let mut agent_ids: Vec<i64> = ctx.sde.agents.keys().copied().take(2).collect();
    agent_ids.sort_unstable();
    assert_eq!(agent_ids.len(), 2, "expected at least 2 agents in SDE");
    let (ag1_id, ag2_id) = (agent_ids[0], agent_ids[1]);
    let sys_id = ctx.sde.agents[&ag1_id].solar_system_id.unwrap_or(30_000_142);
    let st_id = ctx.sde.agents[&ag1_id].station_id.unwrap_or(60_003_760);
    let mut sess = char_session(char_id, Some(sys_id), Some(st_id));
    let kw = HashMap::new();
    let now = ctx.now_unix();
    let (mid1, mid2) = (21_001_u32, 21_002_u32);

    ctx.db.with_mut_snapshot(|snap| {
        snap.active_missions.insert(
            (char_id, mid1),
            make_active_mission(mid1, char_id, ag1_id, sys_id, "Accepted", None),
        );
        save_mission_meta(snap, char_id, &make_meta(mid1, ag1_id, now + 86_400, Some(now + 86_400)));

        snap.active_missions.insert(
            (char_id, mid2),
            make_active_mission(mid2, char_id, ag2_id, sys_id, "Offered", None),
        );
        save_mission_meta(snap, char_id, &make_meta(mid2, ag2_id, now + 86_400, None));

        save_rnd_research_entries(
            snap,
            char_id,
            &[
                RndResearchEntry {
                    agent_id: ag1_id,
                    research_type_id: 11442,
                    points_per_day: 10.0,
                    started_unix: now,
                    initial_points: 5.0,
                },
                RndResearchEntry {
                    agent_id: ag2_id,
                    research_type_id: 11442,
                    points_per_day: 20.0,
                    started_unix: now,
                    initial_points: 15.0,
                },
            ],
        );
    });

    // Unbound call returns all missions and research entries for the character.
    let unbound = call_agent_mgr(&ctx, &mut sess, "GetMyJournalDetails", &[], &kw)
        .await
        .expect("unbound GetMyJournalDetails");
    let PyRep::Tuple(u_parts) = unbound else { panic!("expected tuple") };
    let PyRep::List(u_missions) = &u_parts[0] else { panic!("expected missions list") };
    let PyRep::List(u_research) = &u_parts[1] else { panic!("expected research list") };
    assert_eq!(u_missions.len(), 2);
    assert_eq!(u_research.len(), 2);

    // Bound call on agentMgr object_id = ag1_id returns only ag1_id's mission and research entry (#1016).
    let bound_ag1 = crate::router::BoundObjectEntry {
        bound_id: 10_009,
        session_id: sess.session_id,
        user_id: 1,
        character_id: char_id,
        service: "agentMgr".into(),
        object_id: ag1_id,
        oid_str: format!("agentMgr:{ag1_id}"),
        ref_id: 1,
    };
    let res_ag1 = crate::services::agents_missions::handle_with_bound(
        &ctx,
        &mut sess,
        Some(&bound_ag1),
        "agentMgr",
        "GetMyJournalDetails",
        &[],
        &kw,
    )
    .await
    .expect("handled")
    .expect("bound GetMyJournalDetails ag1");
    let PyRep::Tuple(b1_parts) = res_ag1 else { panic!("expected tuple") };
    let PyRep::List(b1_missions) = &b1_parts[0] else { panic!("expected missions list") };
    let PyRep::List(b1_research) = &b1_parts[1] else { panic!("expected research list") };
    assert_eq!(b1_missions.len(), 1);
    let PyRep::Tuple(b1_row) = &b1_missions[0] else { panic!("expected 10-tuple b1") };
    assert_eq!(b1_row[4], PyRep::Integer(ag1_id));
    assert_eq!(b1_row[9], PyRep::Integer(mid1 as i64));
    assert_eq!(b1_research.len(), 1);
    assert_eq!(b1_research[0].dict_get("agentID"), Some(&PyRep::Integer(ag1_id)));

    // Bound call on agentMgr object_id = ag2_id returns only ag2_id's mission and research entry.
    let bound_ag2 = crate::router::BoundObjectEntry {
        bound_id: 10_010,
        session_id: sess.session_id,
        user_id: 1,
        character_id: char_id,
        service: "agentMgr".into(),
        object_id: ag2_id,
        oid_str: format!("agentMgr:{ag2_id}"),
        ref_id: 2,
    };
    let res_ag2 = crate::services::agents_missions::handle_with_bound(
        &ctx,
        &mut sess,
        Some(&bound_ag2),
        "agentMgr",
        "GetMyJournalDetails",
        &[],
        &kw,
    )
    .await
    .expect("handled")
    .expect("bound GetMyJournalDetails ag2");
    let PyRep::Tuple(b2_parts) = res_ag2 else { panic!("expected tuple") };
    let PyRep::List(b2_missions) = &b2_parts[0] else { panic!("expected missions list") };
    let PyRep::List(b2_research) = &b2_parts[1] else { panic!("expected research list") };
    assert_eq!(b2_missions.len(), 1);
    let PyRep::Tuple(b2_row) = &b2_missions[0] else { panic!("expected 10-tuple b2") };
    assert_eq!(b2_row[4], PyRep::Integer(ag2_id));
    assert_eq!(b2_row[9], PyRep::Integer(mid2 as i64));
    assert_eq!(b2_research.len(), 1);
    assert_eq!(b2_research[0].dict_get("agentID"), Some(&PyRep::Integer(ag2_id)));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn gh1023_courier_objective_info_emits_station_corp_owners_and_normalizes_for_v24() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    // Naki Vikkiras (3016134) at Ikuchi VI - Moon 6 - Ytiri Storage (60000403, corp 1000004),
    // delivering to Hirtamon VII - Moon 5 - Zainou Biotech Production (60001807, corp 1000016),
    // or fallback to any courier mission if specific IDs are absent in a minimal SDE.
    let agent_id = if ctx.sde.agents.contains_key(&3_016_134) {
        3_016_134
    } else {
        find_standard_agent(&ctx, 1).agent_id
    };
    let agent = ctx.sde.agents.get(&agent_id).expect("agent in sde").clone();
    let origin_sid = agent.station_id.unwrap_or(60_000_403);
    let dest_sid = if ctx.sde.stations.contains_key(&60_001_807) {
        60_001_807
    } else {
        ctx.sde
            .stations
            .keys()
            .copied()
            .find(|&sid| sid != origin_sid)
            .unwrap_or(origin_sid)
    };
    let expected_pickup_owner = ctx
        .sde
        .stations
        .get(&origin_sid)
        .map(|s| s.owner_corporation_id)
        .filter(|&v| v > 0)
        .unwrap_or(agent.corporation_id);
    let expected_dropoff_owner = ctx
        .sde
        .stations
        .get(&dest_sid)
        .map(|s| s.owner_corporation_id)
        .filter(|&v| v > 0)
        .unwrap_or(expected_pickup_owner);
    let sys_id = agent.solar_system_id.unwrap_or(30_000_138);
    let mission_id = ctx
        .sde
        .missions
        .values()
        .filter(|m| {
            let ct = m.content_template.as_deref().unwrap_or_default();
            m.is_courier && !ct.contains("Trade") && !ct.contains("Heraldry")
        })
        .min_by_key(|m| m.mission_id)
        .map(|m| m.mission_id)
        .unwrap_or(72_001);

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&char_id) {
            c.solar_system_id = sys_id;
            c.station_id = Some(origin_sid);
        }
        snap.active_missions.insert(
            (char_id, mission_id),
            make_active_mission(mission_id, char_id, agent_id, sys_id, "Accepted", None),
        );
        save_mission_meta(
            snap,
            char_id,
            &crate::services::agents_missions::agents_standings_arcs::MissionTimingAndCargoMeta {
                mission_id,
                agent_id,
                offered_at_unix: 1_700_000_000,
                offer_expiry_unix: 1_700_086_400,
                accepted_at_unix: Some(1_700_000_100),
                accept_expiry_unix: Some(1_700_086_500),
                bonus_expiry_unix: Some(1_700_001_900),
                completed_at_unix: None,
                required_delivery_type_id: Some(3683),
                required_delivery_qty: 6,
                granted_courier_item_id: None,
                granted_courier_qty: 6,
                is_anomic_burner: false,
                chosen_branch: None,
                destination_station_id: Some(dest_sid),
                origin_station_id: Some(origin_sid),
                offered_level: Some(agent.level),
            },
        );
    });

    let mut sess = char_session(char_id, Some(sys_id), Some(origin_sid));
    let raw_obj = call_agent_mgr(
        &ctx,
        &mut sess,
        "GetMissionObjectiveInfo",
        &[PyRep::Integer(agent_id), PyRep::Integer(mission_id as i64)],
        &HashMap::new(),
    )
    .await
    .expect("raw GetMissionObjectiveInfo");
    let Some(PyRep::List(objs)) = raw_obj.dict_get("objectives") else {
        panic!("expected V24 objectives list");
    };
    assert_eq!(objs.len(), 2);
    let PyRep::Tuple(agent_obj) = &objs[0] else { panic!() };
    let PyRep::Tuple(agent_vals) = &agent_obj[1] else { panic!() };
    assert_eq!(agent_vals[0], PyRep::Integer(agent_id));
    let PyRep::Tuple(trans_obj) = &objs[1] else { panic!() };
    assert_eq!(trans_obj[0], PyRep::String("transport".into()));
    let PyRep::Tuple(trans_vals) = &trans_obj[1] else { panic!() };
    assert_eq!(trans_vals[0], PyRep::Integer(expected_pickup_owner));
    assert_eq!(trans_vals[2], PyRep::Integer(expected_dropoff_owner));
    assert_ne!(trans_vals[0], PyRep::Integer(agent_id));
    assert_ne!(trans_vals[2], PyRep::Integer(agent_id));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn per_mission_bonus_window_accept_and_objective_expiry_zeroes_bonus() {
    let ctx = test_ctx();
    let clock = std::sync::Arc::new(eve_db::clock::VirtualServerClock::with_fixed_base(
        1_800_100_000,
    ));
    ctx.set_clock(clock.clone());
    let char_id = 90_000_001_i64;
    let ag1 = find_standard_agent(&ctx, 1);
    let ag4 = find_standard_agent(&ctx, 4);

    // Seed Offered L1 Patriotic Measures (2 of 5) (13_512 -> 44m = 2_640s) and L4 The Sansha Spies (900 -> 4h = 14_400s)
    let sys1 = ag1.solar_system_id.unwrap();
    let st1 = ag1.station_id.unwrap();
    let mut sess = char_session(char_id, Some(sys1), Some(st1));
    let kw = HashMap::new();
    let base_now = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.clone_grade = 2; // Omega so L4 agent acceptance is allowed
            ch.solar_system_id = sys1;
            ch.station_id = Some(st1);
        }
        snap.active_missions.insert(
            (char_id, 13_512),
            ActiveMissionRecord {
                character_id: char_id,
                mission_id: 13_512,
                agent_id: ag1.agent_id,
                state: "Offered".into(),
                solar_system_id: sys1,
                current_room_id: 1,
                npcs_remaining: 0,
                spawned_npc_ball_ids: vec![],
                bookmark_id: None,
            },
        );
        let mut meta_13512 = make_meta(13_512, ag1.agent_id, base_now + 86_400, None);
        meta_13512.required_delivery_type_id = Some(3683);
        meta_13512.required_delivery_qty = 4;
        meta_13512.granted_courier_item_id = Some(960_512);
        meta_13512.granted_courier_qty = 4;
        meta_13512.origin_station_id = Some(st1);
        meta_13512.destination_station_id = Some(st1);
        save_mission_meta(snap, char_id, &meta_13512);
        snap.items.insert(
            960_512,
            InventoryItemRecord {
                item_id: 960_512,
                type_id: 3683,
                owner_id: char_id,
                location_id: st1,
                flag: InventoryFlag::Hangar,
                quantity: 4,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        snap.item_custom_names
            .insert(960_512, format!("courier_cargo:{char_id}:13512"));
    });

    let acc_13512 = call_agent_mgr(
        &ctx,
        &mut sess,
        "DoAction",
        &[PyRep::Integer(3), PyRep::Integer(ag1.agent_id), PyRep::Integer(13_512)],
        &kw,
    )
    .await
    .expect("accept 13512");
    assert_eq!(acc_13512.dict_get("bonusWindowSecs"), Some(&PyRep::Integer(2_640)));
    assert_eq!(
        acc_13512.dict_get("bonusExpiryUnix"),
        Some(&PyRep::Integer((base_now + 2_640) as i64))
    );

    // Before 2,640s elapse, GetMissionObjectiveInfo shows non-zero Wiki-calibrated bonusReward (25,000 ISK * neg_factor)
    let obj_before = call_agent_mgr(
        &ctx,
        &mut sess,
        "GetMissionObjectiveInfo",
        &[PyRep::Integer(ag1.agent_id), PyRep::Integer(13_512)],
        &kw,
    )
    .await
    .expect("GetMissionObjectiveInfo active");
    let Some(PyRep::List(active_bonus)) = obj_before.dict_get("bonusRewards") else {
        panic!("expected bonusRewards list");
    };
    assert_eq!(active_bonus.len(), 1);
    let PyRep::Tuple(bonus_tup) = &active_bonus[0] else { panic!() };
    assert!(bonus_tup[2].as_i64().unwrap_or(0) >= 25_000);
    assert_eq!(bonus_tup[4], PyRep::Integer(44));

    // Advance 2,641s past 13_512's 44-minute window -> GetMissionObjectiveInfo and Complete both return 0 bonusReward
    eve_db::ServerClock::advance_secs(&*clock, 2_641);
    let obj_after = call_agent_mgr(
        &ctx,
        &mut sess,
        "GetMissionObjectiveInfo",
        &[PyRep::Integer(ag1.agent_id), PyRep::Integer(13_512)],
        &kw,
    )
    .await
    .expect("GetMissionObjectiveInfo expired");
    assert_eq!(obj_after.dict_get("bonusRewards"), Some(&PyRep::List(Vec::new())));
    let done_13512 = call_agent_mgr(
        &ctx,
        &mut sess,
        "DoAction",
        &[PyRep::Integer(6), PyRep::Integer(ag1.agent_id), PyRep::Integer(13_512)],
        &kw,
    )
    .await
    .expect("complete expired 13512");
    assert_eq!(done_13512.dict_get("bonusReward"), Some(&PyRep::Integer(0)));

    // L4 Sansha Spies (900) accepts with its 4h (14,400s) window and pays full 510,000*neg bonus before expiry
    let sys4 = ag4.solar_system_id.unwrap();
    let st4 = ag4.station_id.unwrap();
    let now4 = ctx.now_unix();
    let mut sess4 = char_session(char_id, Some(sys4), Some(st4));
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = sys4;
            ch.station_id = Some(st4);
        }
        snap.skills.remove(&char_id);
        snap.active_missions.insert(
            (char_id, 900),
            ActiveMissionRecord {
                character_id: char_id,
                mission_id: 900,
                agent_id: ag4.agent_id,
                state: "Offered".into(),
                solar_system_id: sys4,
                current_room_id: 1,
                npcs_remaining: 0,
                spawned_npc_ball_ids: vec![],
                bookmark_id: None,
            },
        );
        let mut meta_900 = make_meta(900, ag4.agent_id, now4 + 86_400, None);
        meta_900.offered_level = Some(4);
        save_mission_meta(snap, char_id, &meta_900);
    });
    let acc_900 = call_agent_mgr(
        &ctx,
        &mut sess4,
        "DoAction",
        &[PyRep::Integer(3), PyRep::Integer(ag4.agent_id), PyRep::Integer(900)],
        &kw,
    )
    .await
    .expect("accept 900");
    assert_eq!(acc_900.dict_get("bonusWindowSecs"), Some(&PyRep::Integer(14_400)));
    assert_eq!(
        acc_900.dict_get("bonusExpiryUnix"),
        Some(&PyRep::Integer((now4 + 14_400) as i64))
    );
    let done_900 = call_agent_mgr(
        &ctx,
        &mut sess4,
        "DoAction",
        &[PyRep::Integer(6), PyRep::Integer(ag4.agent_id), PyRep::Integer(900)],
        &kw,
    )
    .await
    .expect("complete 900 within 4h window");
    assert_eq!(done_900.dict_get("iskReward"), Some(&PyRep::Integer(358_000)));
    assert_eq!(done_900.dict_get("bonusReward"), Some(&PyRep::Integer(510_000)));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reaccept_respawning_missing_npcs_preserves_bonus_expiry_deadline() {
    let ctx = test_ctx();
    let clock = std::sync::Arc::new(eve_db::clock::VirtualServerClock::with_fixed_base(
        1_800_200_000,
    ));
    ctx.set_clock(clock.clone());
    let char_id = 90_000_001_i64;
    let ag1 = find_standard_agent(&ctx, 1);
    let sys1 = ag1.solar_system_id.unwrap();
    let st1 = ag1.station_id.unwrap();
    let mut sess = char_session(char_id, Some(sys1), Some(st1));
    let kw = HashMap::new();
    let base_now = ctx.now_unix();

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = sys1;
            ch.station_id = Some(st1);
        }
        snap.skills.remove(&char_id);
        snap.active_missions.insert(
            (char_id, 13_777),
            ActiveMissionRecord {
                character_id: char_id,
                mission_id: 13_777,
                agent_id: ag1.agent_id,
                state: "Offered".into(),
                solar_system_id: sys1,
                current_room_id: 1,
                npcs_remaining: 2,
                spawned_npc_ball_ids: vec![],
                bookmark_id: None,
            },
        );
        save_mission_meta(snap, char_id, &make_meta(13_777, ag1.agent_id, base_now + 86_400, None));
    });

    let acc1 = call_agent_mgr(
        &ctx,
        &mut sess,
        "DoAction",
        &[PyRep::Integer(3), PyRep::Integer(ag1.agent_id), PyRep::Integer(13_777)],
        &kw,
    )
    .await
    .expect("first accept 13_777");
    assert_eq!(acc1.dict_get("bonusWindowSecs"), Some(&PyRep::Integer(3_600)));
    let expected_accept_expiry = base_now + 604_800;
    let expected_bonus_expiry = base_now + 3_600;
    assert_eq!(
        acc1.dict_get("expiryUnix"),
        Some(&PyRep::Integer(expected_accept_expiry as i64))
    );
    assert_eq!(
        acc1.dict_get("bonusExpiryUnix"),
        Some(&PyRep::Integer(expected_bonus_expiry as i64))
    );

    // Despawn NPC balls so `can_respawn_missing` triggers on a later `DoAction(3)` call after bonus expiry.
    let initial_balls = ctx.db.with_snapshot(|snap| {
        snap.active_missions
            .get(&(char_id, 13_777))
            .map(|m| m.spawned_npc_ball_ids.clone())
            .unwrap_or_default()
    });
    assert!(!initial_balls.is_empty());
    let sys_handle = ctx.systems.get_or_spawn(sys1);
    for bid in initial_balls {
        let _ = sys_handle.remove_ball(bid).await;
    }

    eve_db::ServerClock::advance_secs(&*clock, 3_650);
    let acc2 = call_agent_mgr(
        &ctx,
        &mut sess,
        "DoAction",
        &[PyRep::Integer(3), PyRep::Integer(ag1.agent_id), PyRep::Integer(13_777)],
        &kw,
    )
    .await
    .expect("re-accept 13_777 to respawn missing NPCs");
    assert_eq!(
        acc2.dict_get("expiryUnix"),
        Some(&PyRep::Integer(expected_accept_expiry as i64)),
        "re-accept must preserve original accept_expiry_unix"
    );
    assert_eq!(
        acc2.dict_get("bonusExpiryUnix"),
        Some(&PyRep::Integer(expected_bonus_expiry as i64)),
        "re-accept must preserve original bonus_expiry_unix"
    );

    ctx.db.with_snapshot(|snap| {
        let meta = load_mission_meta(snap, char_id, 13_777).expect("meta preserved");
        assert_eq!(meta.accepted_at_unix, Some(base_now));
        assert_eq!(meta.accept_expiry_unix, Some(expected_accept_expiry));
        assert_eq!(meta.bonus_expiry_unix, Some(expected_bonus_expiry));
    });

    // Clear the respawned NPCs and complete -> expired bonus deadline must still yield bonusReward == 0.
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(m) = snap.active_missions.get_mut(&(char_id, 13_777)) {
            m.npcs_remaining = 0;
        }
    });
    let done = call_agent_mgr(
        &ctx,
        &mut sess,
        "DoAction",
        &[PyRep::Integer(6), PyRep::Integer(ag1.agent_id), PyRep::Integer(13_777)],
        &kw,
    )
    .await
    .expect("complete respawned 13_777");
    assert_eq!(done.dict_get("bonusReward"), Some(&PyRep::Integer(0)));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn get_mission_briefing_info_uses_mission_specific_text_instead_of_fixed_template() {
    let mut ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let kw = HashMap::new();

    // Deterministically pick SDE templates across mission categories so each yields its own briefing.
    let combat_tmpl = ctx
        .sde
        .missions
        .values()
        .filter(|m| {
            let ct = m.content_template.as_deref().unwrap_or_default();
            !m.is_courier && ct.contains("BasicKill") && !ct.contains("EpicArc")
        })
        .min_by_key(|m| m.mission_id)
        .cloned()
        .expect("combat mission in SDE");
    let courier_tmpl = ctx
        .sde
        .missions
        .values()
        .filter(|m| {
            let ct = m.content_template.as_deref().unwrap_or_default();
            m.is_courier && !ct.contains("Trade") && !ct.contains("Heraldry") && !ct.contains("EpicArc")
        })
        .min_by_key(|m| m.mission_id)
        .cloned()
        .expect("courier mission in SDE");
    let mining_tmpl = ctx
        .sde
        .missions
        .values()
        .filter(|m| {
            m.content_template
                .as_deref()
                .unwrap_or_default()
                .contains("Mining")
        })
        .min_by_key(|m| m.mission_id)
        .cloned()
        .expect("mining mission in SDE");
    let trade_tmpl = ctx
        .sde
        .missions
        .values()
        .filter(|m| {
            let ct = m.content_template.as_deref().unwrap_or_default();
            ct.contains("Trade") && !ct.contains("EpicArc")
        })
        .min_by_key(|m| m.mission_id)
        .cloned()
        .expect("trade mission in SDE");

    // Add a synthetic mission with explicit non-templatized prose in content_template
    // to verify custom mission text in SDE is surfaced directly.
    let custom_mid = 990_204_u32;
    let custom_prose = "Recover the encrypted Caldari courier beacon from the derelict cruiser wreck before Syndicate salvagers arrive.";
    std::sync::Arc::make_mut(&mut ctx.sde).missions.insert(
        custom_mid,
        eve_sde::MissionTemplate {
            mission_id: custom_mid,
            title: "Recovered Beacon".into(),
            is_courier: false,
            lp_alpha: 100,
            lp_omega: 150,
            has_standing_rewards: true,
            content_template: Some(custom_prose.into()),
            dungeon_id: None,
        },
    );

    let ag = find_standard_agent(&ctx, 1);
    let sys_id = ag.solar_system_id.unwrap();
    let origin_sid = ag.station_id.unwrap();
    let courier_details = resolve_courier_mission_details(&ctx, &courier_tmpl, &ag)
        .expect("courier details");
    let mining_details = resolve_mining_mission_details(&ctx, &mining_tmpl);
    let trade_details = resolve_trade_mission_details(&ctx, &trade_tmpl);
    let combat_enc = resolve_security_mission_encounter(&ctx, &combat_tmpl, &ag, sys_id);

    let mut sess = char_session(char_id, Some(sys_id), Some(origin_sid));
    let now = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        for (mid, req_tid, req_qty, dest_sid) in [
            (combat_tmpl.mission_id, None, 0, None),
            (
                courier_tmpl.mission_id,
                Some(courier_details.cargo_type_id),
                courier_details.cargo_quantity,
                Some(courier_details.destination_station_id),
            ),
            (
                mining_tmpl.mission_id,
                Some(mining_details.ore_type_id),
                mining_details.required_units,
                None,
            ),
            (
                trade_tmpl.mission_id,
                Some(trade_details.trade_type_id),
                trade_details.trade_quantity,
                None,
            ),
            (custom_mid, None, 0, None),
        ] {
            snap.active_missions.insert(
                (char_id, mid),
                make_active_mission(mid, char_id, ag.agent_id, sys_id, "Offered", None),
            );
            let mut mt = make_meta(mid, ag.agent_id, now + 86_400, None);
            mt.required_delivery_type_id = req_tid;
            mt.required_delivery_qty = req_qty;
            mt.origin_station_id = Some(origin_sid);
            mt.destination_station_id = dest_sid;
            save_mission_meta(snap, char_id, &mt);
        }
    });

    let combat_rep = call_agent_mgr(
        &ctx,
        &mut sess,
        "GetMissionBriefingInfo",
        &[PyRep::Integer(ag.agent_id), PyRep::Integer(combat_tmpl.mission_id as i64)],
        &kw,
    )
    .await
    .expect("combat briefing");
    let combat_str = combat_rep.dict_get("briefing").and_then(|v| v.as_str()).unwrap();
    assert!(!combat_str.contains("Execute objectives in system"));
    assert!(combat_str.contains(&combat_enc.dungeon_name));
    assert!(combat_str.contains(&combat_tmpl.title));

    let courier_rep = call_agent_mgr(
        &ctx,
        &mut sess,
        "GetMissionBriefingInfo",
        &[PyRep::Integer(ag.agent_id), PyRep::Integer(courier_tmpl.mission_id as i64)],
        &kw,
    )
    .await
    .expect("courier briefing");
    let courier_str = courier_rep.dict_get("briefing").and_then(|v| v.as_str()).unwrap();
    assert!(!courier_str.contains("Execute objectives in system"));
    assert!(courier_str.contains(&courier_tmpl.title));
    if let Some(cargo_name) = ctx.sde.types.get(&courier_details.cargo_type_id).map(|t| t.name.as_str()) {
        assert!(courier_str.contains(cargo_name), "courier briefing must name cargo {cargo_name}: {courier_str}");
    }
    if let Some(dest_name) = ctx.sde.stations.get(&courier_details.destination_station_id).map(|s| s.name.as_str()) {
        assert!(courier_str.contains(dest_name), "courier briefing must name dest {dest_name}: {courier_str}");
    }

    let mining_rep = call_agent_mgr(
        &ctx,
        &mut sess,
        "GetMissionBriefingInfo",
        &[PyRep::Integer(ag.agent_id), PyRep::Integer(mining_tmpl.mission_id as i64)],
        &kw,
    )
    .await
    .expect("mining briefing");
    let mining_str = mining_rep.dict_get("briefing").and_then(|v| v.as_str()).unwrap();
    assert!(!mining_str.contains("Execute objectives in system"));
    assert!(mining_str.contains(&mining_tmpl.title));
    assert!(mining_str.contains(&mining_details.required_units.to_string()));

    let trade_rep = call_agent_mgr(
        &ctx,
        &mut sess,
        "GetMissionBriefingInfo",
        &[PyRep::Integer(ag.agent_id), PyRep::Integer(trade_tmpl.mission_id as i64)],
        &kw,
    )
    .await
    .expect("trade briefing");
    let trade_str = trade_rep.dict_get("briefing").and_then(|v| v.as_str()).unwrap();
    assert!(!trade_str.contains("Execute objectives in system"));
    assert!(trade_str.contains(&trade_tmpl.title));

    let custom_rep = call_agent_mgr(
        &ctx,
        &mut sess,
        "GetMissionBriefingInfo",
        &[PyRep::Integer(ag.agent_id), PyRep::Integer(custom_mid as i64)],
        &kw,
    )
    .await
    .expect("custom prose briefing");
    let custom_str = custom_rep.dict_get("briefing").and_then(|v| v.as_str()).unwrap();
    assert!(custom_str.contains(custom_prose), "custom prose must be returned: {custom_str}");

    // Distinct missions produce distinct briefings.
    assert_ne!(combat_str, courier_str);
    assert_ne!(courier_str, mining_str);
    assert_ne!(mining_str, trade_str);

    // Re-opening an existing Offered mission via DoAction(2) also uses the rich briefing builder.
    ctx.db.with_mut_snapshot(|snap| {
        snap.active_missions
            .retain(|(cid, mid), _| !(*cid == char_id && *mid != combat_tmpl.mission_id));
    });
    let reoffer = call_agent_mgr(
        &ctx,
        &mut sess,
        "DoAction",
        &[PyRep::Integer(2), PyRep::Integer(ag.agent_id)],
        &kw,
    )
    .await
    .expect("re-offer via DoAction(2)");
    let reoffer_briefing = reoffer.dict_get("briefing").and_then(|v| v.as_str()).unwrap();
    assert_eq!(reoffer_briefing, combat_str);
    assert!(!reoffer_briefing.contains("assignment:"));

    // Negative path: unseeded mission/agent ID fails cleanly with MissionNotActive once cleared.
    ctx.db.with_mut_snapshot(|snap| {
        snap.active_missions.retain(|(cid, _), _| *cid != char_id);
    });
    let missing_err = call_agent_mgr(
        &ctx,
        &mut sess,
        "GetMissionBriefingInfo",
        &[PyRep::Integer(999_888_777)],
        &kw,
    )
    .await
    .expect_err("unknown mission must fail");
    assert_eq!(missing_err.code, "MissionNotActive");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn briefing_distinguishes_data_center_cosmos_heraldry_div23_mining_and_initial_offer() {
    let mut ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let kw = HashMap::new();
    let now = ctx.now_unix();

    let combat_tmpl = ctx
        .sde
        .missions
        .values()
        .filter(|m| {
            let ct = m.content_template.as_deref().unwrap_or_default();
            !m.is_courier && ct.contains("BasicKill") && !ct.contains("EpicArc")
        })
        .min_by_key(|m| m.mission_id)
        .cloned()
        .expect("combat mission in SDE");

    // 1. Data Center (agent_type_id == 5): initial DoAction(2) offer and GetMissionBriefingInfo
    // both emit Data Center Pirate Tag turn-in copy, even on a BasicKill template.
    let dc_ag = ctx
        .sde
        .agents
        .values()
        .filter(|a| a.agent_type_id == 5 && a.level == 1 && a.solar_system_id.is_some() && a.station_id.is_some())
        .min_by_key(|a| a.agent_id)
        .cloned()
        .expect("L1 Data Center agent in SDE");
    let dc_sys = dc_ag.solar_system_id.unwrap();
    let dc_st = dc_ag.station_id.unwrap();
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = dc_sys;
            ch.station_id = Some(dc_st);
            ch.standings.insert(dc_ag.agent_id, 10.0);
            ch.standings.insert(dc_ag.corporation_id, 10.0);
        }
    });
    let mut dc_sess = char_session(char_id, Some(dc_sys), Some(dc_st));
    let dc_offer = call_agent_mgr(
        &ctx,
        &mut dc_sess,
        "DoAction",
        &[PyRep::Integer(2), PyRep::Integer(dc_ag.agent_id)],
        &kw,
    )
    .await
    .expect("Data Center DoAction(2)");
    let dc_mid = dc_offer.dict_get("missionID").and_then(|v| v.as_i64()).unwrap() as u32;
    let dc_offer_brief = dc_offer.dict_get("briefing").and_then(|v| v.as_str()).unwrap().to_string();
    let dc_tag_name = ctx.sde.types.get(&17202).map_or("Pirate Tag", |t| t.name.as_str());
    assert!(dc_offer_brief.contains("Data Center commission"), "unexpected Data Center briefing: {dc_offer_brief}");
    assert!(dc_offer_brief.contains(dc_tag_name), "expected {dc_tag_name} in {dc_offer_brief}");
    assert!(!dc_offer_brief.contains("Data Center Agent "));
    let dc_info = call_agent_mgr(
        &ctx,
        &mut dc_sess,
        "GetMissionBriefingInfo",
        &[PyRep::Integer(dc_ag.agent_id), PyRep::Integer(dc_mid as i64)],
        &kw,
    )
    .await
    .expect("Data Center GetMissionBriefingInfo");
    assert_eq!(dc_info.dict_get("briefing").and_then(|v| v.as_str()), Some(dc_offer_brief.as_str()));

    ctx.db.with_mut_snapshot(|snap| {
        snap.active_missions.insert(
            (char_id, combat_tmpl.mission_id),
            make_active_mission(combat_tmpl.mission_id, char_id, dc_ag.agent_id, dc_sys, "Offered", None),
        );
        let mut mt = make_meta(combat_tmpl.mission_id, dc_ag.agent_id, now + 86_400, None);
        mt.required_delivery_type_id = Some(17202);
        mt.required_delivery_qty = 1;
        mt.origin_station_id = Some(dc_st);
        save_mission_meta(snap, char_id, &mt);
    });
    let dc_kill_info = call_agent_mgr(
        &ctx,
        &mut dc_sess,
        "GetMissionBriefingInfo",
        &[PyRep::Integer(dc_ag.agent_id), PyRep::Integer(combat_tmpl.mission_id as i64)],
        &kw,
    )
    .await
    .expect("Data Center BasicKill template briefing");
    let dc_kill_str = dc_kill_info.dict_get("briefing").and_then(|v| v.as_str()).unwrap();
    assert!(dc_kill_str.contains("Data Center commission"), "Data Center must not fall back to combat: {dc_kill_str}");
    assert!(dc_kill_str.contains(dc_tag_name), "expected {dc_tag_name} in {dc_kill_str}");

    // 2. COSMOS (agent_type_id == 8): must NOT hit Data Center branch; flows into security combat briefing.
    let cosmos_ag = ctx
        .sde
        .agents
        .values()
        .filter(|a| a.agent_type_id == 8 && a.solar_system_id.is_some() && a.station_id.is_some())
        .min_by_key(|a| a.agent_id)
        .cloned()
        .expect("COSMOS agent in SDE");
    let cosmos_sys = cosmos_ag.solar_system_id.unwrap();
    let cosmos_st = cosmos_ag.station_id.unwrap();
    let cosmos_enc = resolve_security_mission_encounter(&ctx, &combat_tmpl, &cosmos_ag, cosmos_sys);
    ctx.db.with_mut_snapshot(|snap| {
        snap.active_missions.retain(|(cid, _), _| *cid != char_id);
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = cosmos_sys;
            ch.station_id = Some(cosmos_st);
        }
        snap.active_missions.insert(
            (char_id, combat_tmpl.mission_id),
            make_active_mission(combat_tmpl.mission_id, char_id, cosmos_ag.agent_id, cosmos_sys, "Offered", None),
        );
        let mut mt = make_meta(combat_tmpl.mission_id, cosmos_ag.agent_id, now + 86_400, None);
        mt.origin_station_id = Some(cosmos_st);
        save_mission_meta(snap, char_id, &mt);
    });
    let mut cosmos_sess = char_session(char_id, Some(cosmos_sys), Some(cosmos_st));
    let cosmos_info = call_agent_mgr(
        &ctx,
        &mut cosmos_sess,
        "GetMissionBriefingInfo",
        &[PyRep::Integer(cosmos_ag.agent_id), PyRep::Integer(combat_tmpl.mission_id as i64)],
        &kw,
    )
    .await
    .expect("COSMOS GetMissionBriefingInfo");
    let cosmos_str = cosmos_info.dict_get("briefing").and_then(|v| v.as_str()).unwrap();
    assert!(!cosmos_str.contains("Data Center"), "COSMOS must not use Data Center copy: {cosmos_str}");
    assert!(!cosmos_str.contains(dc_tag_name));
    assert!(cosmos_str.contains(&cosmos_enc.dungeon_name));
    assert!(cosmos_str.contains(&combat_tmpl.title));

    // 3. Division 23 Mining predicate: matches non-kill templates without "Mining" in content_template,
    // while preserving combat briefing for BasicKill templates on the same division-23 agent.
    let div23_ag = ctx
        .sde
        .agents
        .values()
        .filter(|a| a.division_id == 23 && a.solar_system_id.is_some() && a.station_id.is_some())
        .min_by_key(|a| a.agent_id)
        .cloned()
        .expect("division 23 agent in SDE");
    let d23_sys = div23_ag.solar_system_id.unwrap();
    let d23_st = div23_ag.station_id.unwrap();
    let div23_mid = 990_205_u32;
    let div23_tmpl = eve_sde::MissionTemplate {
        mission_id: div23_mid,
        title: "Belt Survey Operation".into(),
        is_courier: false,
        lp_alpha: 100,
        lp_omega: 150,
        has_standing_rewards: true,
        content_template: Some("agent.missionTemplatizedContent_GenericExtraction".into()),
        dungeon_id: None,
    };
    let div23_details = resolve_mining_mission_details(&ctx, &div23_tmpl);
    std::sync::Arc::make_mut(&mut ctx.sde).missions.insert(div23_mid, div23_tmpl);
    ctx.db.with_mut_snapshot(|snap| {
        snap.active_missions.retain(|(cid, _), _| *cid != char_id);
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = d23_sys;
            ch.station_id = Some(d23_st);
        }
        snap.active_missions.insert(
            (char_id, div23_mid),
            make_active_mission(div23_mid, char_id, div23_ag.agent_id, d23_sys, "Offered", None),
        );
        let mut mt_m = make_meta(div23_mid, div23_ag.agent_id, now + 86_400, None);
        mt_m.required_delivery_type_id = Some(div23_details.ore_type_id);
        mt_m.required_delivery_qty = div23_details.required_units;
        mt_m.origin_station_id = Some(d23_st);
        save_mission_meta(snap, char_id, &mt_m);

        snap.active_missions.insert(
            (char_id, combat_tmpl.mission_id),
            make_active_mission(combat_tmpl.mission_id, char_id, div23_ag.agent_id, d23_sys, "Offered", None),
        );
        let mut mt_k = make_meta(combat_tmpl.mission_id, div23_ag.agent_id, now + 86_400, None);
        mt_k.origin_station_id = Some(d23_st);
        save_mission_meta(snap, char_id, &mt_k);
    });
    let mut d23_sess = char_session(char_id, Some(d23_sys), Some(d23_st));
    let d23_mining_str = call_agent_mgr(
        &ctx,
        &mut d23_sess,
        "GetMissionBriefingInfo",
        &[PyRep::Integer(div23_ag.agent_id), PyRep::Integer(div23_mid as i64)],
        &kw,
    )
    .await
    .expect("div23 generic mining briefing")
    .dict_get("briefing")
    .and_then(|v| v.as_str())
    .unwrap()
    .to_string();
    assert!(d23_mining_str.contains("Mining operation"), "div23 non-kill mission must get mining briefing: {d23_mining_str}");
    assert!(d23_mining_str.contains(&div23_details.required_units.to_string()));
    let d23_kill_str = call_agent_mgr(
        &ctx,
        &mut d23_sess,
        "GetMissionBriefingInfo",
        &[PyRep::Integer(div23_ag.agent_id), PyRep::Integer(combat_tmpl.mission_id as i64)],
        &kw,
    )
    .await
    .expect("div23 BasicKill briefing")
    .dict_get("briefing")
    .and_then(|v| v.as_str())
    .unwrap()
    .to_string();
    assert!(!d23_kill_str.contains("Mining operation"), "div23 BasicKill must remain security combat: {d23_kill_str}");

    // 4. Heraldry (is_courier = 1 + HeraldryMission) & Courier (division_id = 22) & R&D (agent_type_id = 4):
    // initial DoAction(2) offer text matches GetMissionBriefingInfo identically.
    let her_ag = ctx
        .sde
        .agents
        .values()
        .filter(|a| (a.agent_type_id == 13 || a.division_id == 37) && a.solar_system_id.is_some() && a.station_id.is_some())
        .min_by_key(|a| a.agent_id)
        .cloned()
        .expect("Heraldry agent in SDE");
    let her_sys = her_ag.solar_system_id.unwrap();
    let her_st = her_ag.station_id.unwrap();
    ctx.db.with_mut_snapshot(|snap| {
        snap.active_missions.retain(|(cid, _), _| *cid != char_id);
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = her_sys;
            ch.station_id = Some(her_st);
            ch.standings.insert(her_ag.agent_id, 10.0);
        }
    });
    let mut her_sess = char_session(char_id, Some(her_sys), Some(her_st));
    let her_offer = call_agent_mgr(
        &ctx,
        &mut her_sess,
        "DoAction",
        &[PyRep::Integer(2), PyRep::Integer(her_ag.agent_id)],
        &kw,
    )
    .await
    .expect("Heraldry DoAction(2)");
    let her_mid = her_offer.dict_get("missionID").and_then(|v| v.as_i64()).unwrap() as u32;
    let her_brief = her_offer.dict_get("briefing").and_then(|v| v.as_str()).unwrap().to_string();
    assert!(her_brief.contains("Heraldry commission"), "Heraldry offer must use heraldry copy: {her_brief}");
    let her_info = call_agent_mgr(
        &ctx,
        &mut her_sess,
        "GetMissionBriefingInfo",
        &[PyRep::Integer(her_ag.agent_id), PyRep::Integer(her_mid as i64)],
        &kw,
    )
    .await
    .expect("Heraldry GetMissionBriefingInfo");
    assert_eq!(her_info.dict_get("briefing").and_then(|v| v.as_str()), Some(her_brief.as_str()));

    let dist_ag = ctx
        .sde
        .agents
        .values()
        .filter(|a| a.agent_type_id == 2 && a.division_id == 22 && a.level == 1 && a.solar_system_id.is_some() && a.station_id.is_some())
        .min_by_key(|a| a.agent_id)
        .cloned()
        .expect("L1 Distribution agent in SDE");
    let dist_sys = dist_ag.solar_system_id.unwrap();
    let dist_st = dist_ag.station_id.unwrap();
    ctx.db.with_mut_snapshot(|snap| {
        snap.active_missions.retain(|(cid, _), _| *cid != char_id);
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = dist_sys;
            ch.station_id = Some(dist_st);
        }
    });
    let mut dist_sess = char_session(char_id, Some(dist_sys), Some(dist_st));
    let dist_offer = call_agent_mgr(
        &ctx,
        &mut dist_sess,
        "DoAction",
        &[PyRep::Integer(2), PyRep::Integer(dist_ag.agent_id)],
        &kw,
    )
    .await
    .expect("Distribution DoAction(2)");
    let dist_mid = dist_offer.dict_get("missionID").and_then(|v| v.as_i64()).unwrap() as u32;
    let dist_brief = dist_offer.dict_get("briefing").and_then(|v| v.as_str()).unwrap().to_string();
    assert!(dist_brief.contains("Courier contract"), "initial courier offer must use Courier contract builder: {dist_brief}");
    assert!(!dist_brief.contains("operation:"));
    let dist_info = call_agent_mgr(
        &ctx,
        &mut dist_sess,
        "GetMissionBriefingInfo",
        &[PyRep::Integer(dist_ag.agent_id), PyRep::Integer(dist_mid as i64)],
        &kw,
    )
    .await
    .expect("Distribution GetMissionBriefingInfo");
    assert_eq!(dist_info.dict_get("briefing").and_then(|v| v.as_str()), Some(dist_brief.as_str()));

    let rnd_ag = ctx
        .sde
        .agents
        .values()
        .filter(|a| a.agent_type_id == 4 && a.level == 1 && a.solar_system_id.is_some() && a.station_id.is_some())
        .min_by_key(|a| a.agent_id)
        .cloned()
        .expect("L1 R&D agent in SDE");
    let rnd_sys = rnd_ag.solar_system_id.unwrap();
    let rnd_st = rnd_ag.station_id.unwrap();
    ctx.db.with_mut_snapshot(|snap| {
        snap.active_missions.retain(|(cid, _), _| *cid != char_id);
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = rnd_sys;
            ch.station_id = Some(rnd_st);
            ch.standings.insert(rnd_ag.agent_id, 10.0);
        }
        save_rnd_research_entries(
            snap,
            char_id,
            &[RndResearchEntry {
                agent_id: rnd_ag.agent_id,
                research_type_id: 11442,
                points_per_day: 50.0,
                started_unix: now.saturating_sub(86_400),
                initial_points: 100.0,
            }],
        );
    });
    let mut rnd_sess = char_session(char_id, Some(rnd_sys), Some(rnd_st));
    let rnd_offer = call_agent_mgr(
        &ctx,
        &mut rnd_sess,
        "DoAction",
        &[PyRep::Integer(2), PyRep::Integer(rnd_ag.agent_id)],
        &kw,
    )
    .await
    .expect("R&D DoAction(2)");
    let rnd_mid = rnd_offer.dict_get("missionID").and_then(|v| v.as_i64()).unwrap() as u32;
    let rnd_brief = rnd_offer.dict_get("briefing").and_then(|v| v.as_str()).unwrap().to_string();
    assert!(rnd_brief.contains("Research assignment"), "initial R&D offer must use Research assignment builder: {rnd_brief}");
    assert!(!rnd_brief.contains("R&D Agent "));
    let rnd_info = call_agent_mgr(
        &ctx,
        &mut rnd_sess,
        "GetMissionBriefingInfo",
        &[PyRep::Integer(rnd_ag.agent_id), PyRep::Integer(rnd_mid as i64)],
        &kw,
    )
    .await
    .expect("R&D GetMissionBriefingInfo");
    assert_eq!(rnd_info.dict_get("briefing").and_then(|v| v.as_str()), Some(rnd_brief.as_str()));
}

// Regression coverage for #1367 (V24 dungeon bookmark locations and Telecom dialogue).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn v24_dungeon_bookmark_locations_and_telecom_dialogue_are_warpable_and_human_readable() {
    let mut ctx = test_ctx();
    ctx.set_clock(std::sync::Arc::new(
        eve_db::clock::VirtualServerClock::with_fixed_base(1_800_000_000),
    ));
    let char_id = 90_000_001_i64;
    let ag = find_standard_agent(&ctx, 1);
    let sys_id = ag.solar_system_id.unwrap();
    let st_id = ag.station_id.unwrap();
    let now = ctx.now_unix();
    ctx.db
        .acquire_online_lock(1, 501, Some(char_id), true, now)
        .expect("online lock");

    let mid_kill = 999_875_u32;
    let mid_other = 999_876_u32;
    let mut sde = (*ctx.sde).clone();
    let dun_id = sde.dungeons.keys().copied().min();
    sde.missions.insert(
        mid_kill,
        eve_sde::MissionTemplate {
            mission_id: mid_kill,
            title: "The Guristas Spies".into(),
            is_courier: false,
            lp_alpha: 100,
            lp_omega: 100,
            has_standing_rewards: true,
            content_template: Some("agent.missionTemplatizedContent_BasicKillMission".into()),
            dungeon_id: dun_id,
        },
    );
    sde.missions.insert(
        mid_other,
        eve_sde::MissionTemplate {
            mission_id: mid_other,
            title: "Other Patrol".into(),
            is_courier: false,
            lp_alpha: 100,
            lp_omega: 100,
            has_standing_rewards: true,
            content_template: Some("agent.missionTemplatizedContent_BasicKillMission".into()),
            dungeon_id: dun_id,
        },
    );
    ctx.sde = std::sync::Arc::new(sde);

    ctx.db.with_mut_snapshot(|snap| {
        snap.active_missions.retain(|(cid, _), _| *cid != char_id);
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = sys_id;
            ch.station_id = Some(st_id);
            ch.standings.insert(ag.agent_id, 10.0);
        }
        snap.active_missions.insert(
            (char_id, mid_kill),
            make_active_mission(mid_kill, char_id, ag.agent_id, sys_id, "Offered", None),
        );
        let mut offer_meta = make_meta(mid_kill, ag.agent_id, now + 3_600, None);
        offer_meta.offered_at_unix = now.saturating_sub(60);
        offer_meta.origin_station_id = Some(st_id);
        save_mission_meta(snap, char_id, &offer_meta);

        let other_bm_id = 888_999_002_i64;
        snap.bookmarks.insert(
            other_bm_id,
            BookmarkRecord {
                bookmark_id: other_bm_id,
                owner_id: char_id,
                solar_system_id: sys_id,
                label: "Other Pocket".into(),
                x: 9_000.0,
                y: 8_000.0,
                z: 7_000.0,
                item_id: None,
                memo: "Mission Pocket Destination".into(),
                folder_id: None,
                is_shared: false,
            },
        );
        snap.active_missions.insert(
            (char_id, mid_other),
            make_active_mission(mid_other, char_id, ag.agent_id, sys_id, "Accepted", Some(other_bm_id)),
        );
        save_mission_meta(
            snap,
            char_id,
            &make_meta(mid_other, ag.agent_id, now + 3_600, Some(now + 7_200)),
        );
    });

    let mut sess = char_session(char_id, Some(sys_id), Some(st_id));
    let kw = HashMap::new();
    let _ = ctx.drain_notifications_for_char(char_id);

    call_agent_mgr(
        &ctx,
        &mut sess,
        "DoAction",
        &[PyRep::Integer(3), PyRep::Integer(ag.agent_id), PyRep::Integer(mid_kill as i64)],
        &kw,
    )
    .await
    .expect("DoAction(3) accept must succeed");

    let accept_notifs = ctx.drain_notifications_for_char(char_id);
    let accept_dialogue = accept_notifs
        .iter()
        .find(|n| n.event == "OnIncomingAgentMessage")
        .expect("accept must emit OnIncomingAgentMessage for Telecom");
    let accept_args_dbg = format!("{:?}", accept_dialogue.args);
    assert!(
        accept_args_dbg.contains("UI/Agents/Dialogue/StandardMissionAccepted"),
        "accept dialogue must use StandardMissionAccepted label: {accept_args_dbg}"
    );
    assert!(
        accept_args_dbg.contains("The Guristas Spies")
            && !accept_args_dbg.contains("agent.missionTemplatizedContent_"),
        "accept dialogue must include readable mission title instead of raw content_template key: {accept_args_dbg}"
    );
    let accept_tx = accept_notifs
        .iter()
        .find(|n| n.event == "OnIncomingTransmission")
        .expect("accept must emit OnIncomingTransmission for Telecom");
    let accept_tx_dbg = format!("{:?}", accept_tx.args);
    let accept_tx_text = accept_tx
        .args
        .first()
        .and_then(|kv| kv.dict_get("text"))
        .and_then(PyRep::as_str)
        .unwrap_or("");
    assert!(
        accept_tx_text.contains("The Guristas Spies")
            && accept_tx_text.contains("report back")
            && !accept_tx_text.contains("agent.missionTemplatizedContent_"),
        "accept transmission text must emit human-readable briefing prose instead of raw content_template key: {accept_tx_dbg}"
    );

    let (bm_id, bx, by, bz) = ctx.db.with_snapshot(|snap| {
        let m = snap
            .active_missions
            .get(&(char_id, mid_kill))
            .expect("accepted mission record");
        let bmid = m.bookmark_id.expect("accept must create pocket bookmark");
        let bm = snap.bookmarks.get(&bmid).expect("pocket bookmark stored");
        (bmid, bm.x, bm.y, bm.z)
    });

    // GetMyJournalDetails emits creatorID/created/expiry/deadspace/note and never leaks mid_other's bookmark.
    let journal = call_agent_mgr(&ctx, &mut sess, "GetMyJournalDetails", &[], &kw)
        .await
        .expect("GetMyJournalDetails");
    let PyRep::Tuple(j_parts) = journal else {
        panic!("expected tuple from GetMyJournalDetails");
    };
    let PyRep::List(ref j_missions) = j_parts[0] else {
        panic!("expected missions list");
    };
    let PyRep::Tuple(kill_row) = j_missions
        .iter()
        .find(|r| {
            matches!(r, PyRep::Tuple(cols) if cols.get(9) == Some(&PyRep::Integer(mid_kill as i64)))
        })
        .expect("mid_kill present in journal")
    else {
        panic!("expected 10-tuple kill row");
    };
    let PyRep::List(bms) = &kill_row[6] else {
        panic!("expected bookmarks list");
    };
    assert_eq!(bms.len(), 1, "must include only mid_kill's pocket bookmark");
    assert_eq!(bms[0].dict_get("bookmarkID"), Some(&PyRep::Integer(bm_id)));
    assert_eq!(bms[0].dict_get("creatorID"), Some(&PyRep::None));
    assert_eq!(bms[0].dict_get("expiry"), Some(&PyRep::None));
    assert_eq!(bms[0].dict_get("deadspace"), Some(&PyRep::Integer(1)));
    assert_eq!(bms[0].dict_get("note"), Some(&PyRep::String(String::new())));
    assert_eq!(bms[0].dict_get("itemID"), Some(&PyRep::None));
    assert_eq!(bms[0].dict_get("typeID"), Some(&PyRep::Integer(5)));
    assert_eq!(bms[0].dict_get("locationID"), Some(&PyRep::Integer(sys_id as i64)));
    assert_eq!(bms[0].dict_get("locationType"), Some(&PyRep::String("dungeon".into())));
    let expected_ft = (now as i64 + 11_644_473_600) * 10_000_000;
    assert_eq!(bms[0].dict_get("created"), Some(&PyRep::Integer(expected_ft)));

    // GetMissionObjectiveInfo and GetMissionBriefingInfo expose warpable dungeon location + coords.
    for method in ["GetMissionObjectiveInfo", "GetMissionBriefingInfo"] {
        let info = call_agent_mgr(
            &ctx,
            &mut sess,
            method,
            &[PyRep::Integer(ag.agent_id), PyRep::Integer(mid_kill as i64)],
            &kw,
        )
        .await
        .expect(method);
        let Some(PyRep::List(locs)) = info.dict_get("locations") else {
            panic!("{method} must include locations list");
        };
        assert!(
            locs.iter()
                .any(|l| l.dict_get("locationType") == Some(&PyRep::String("agenthomebase".into()))),
            "{method} locations must keep agenthomebase"
        );
        let dun_loc = locs
            .iter()
            .find(|l| l.dict_get("locationType") == Some(&PyRep::String("dungeon".into())))
            .expect("locations must include warpable dungeon entry");
        assert_eq!(dun_loc.dict_get("locationID"), Some(&PyRep::Integer(sys_id as i64)));
        assert_eq!(dun_loc.dict_get("solarsystemID"), Some(&PyRep::Integer(sys_id as i64)));
        assert_eq!(dun_loc.dict_get("stationID"), Some(&PyRep::None));
        assert_eq!(dun_loc.dict_get("typeID"), Some(&PyRep::Integer(5)));
        assert_eq!(dun_loc.dict_get("BookmarkID"), Some(&PyRep::Integer(bm_id)));
        assert_eq!(
            dun_loc.dict_get("coords"),
            Some(&PyRep::Tuple(vec![
                PyRep::Real(bx),
                PyRep::Real(by),
                PyRep::Real(bz),
            ]))
        );

        let Some(PyRep::List(dungeons)) = info.dict_get("dungeons") else {
            panic!("{method} must include dungeons list");
        };
        if dun_id.is_some() {
            assert_eq!(dungeons.len(), 1);
            assert_eq!(dungeons[0].dict_get("locationID"), Some(&PyRep::Integer(sys_id as i64)));
            assert_eq!(dungeons[0].dict_get("stationID"), Some(&PyRep::None));
            assert_eq!(dungeons[0].dict_get("typeID"), Some(&PyRep::Integer(5)));
            let inner_loc = dungeons[0]
                .dict_get("location")
                .expect("dungeons[0].location present");
            assert_eq!(
                inner_loc.dict_get("locationType"),
                Some(&PyRep::String("dungeon".into()))
            );
            assert_eq!(
                inner_loc.dict_get("coords"),
                Some(&PyRep::Tuple(vec![
                    PyRep::Real(bx),
                    PyRep::Real(by),
                    PyRep::Real(bz),
                ]))
            );
        }
    }

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(m) = snap.active_missions.get_mut(&(char_id, mid_kill)) {
            m.npcs_remaining = 0;
        }
    });
    let _ = ctx.drain_notifications_for_char(char_id);
    call_agent_mgr(
        &ctx,
        &mut sess,
        "DoAction",
        &[PyRep::Integer(6), PyRep::Integer(ag.agent_id), PyRep::Integer(mid_kill as i64)],
        &kw,
    )
    .await
    .expect("DoAction(6) complete must succeed");

    let complete_notifs = ctx.drain_notifications_for_char(char_id);
    let complete_dialogue = complete_notifs
        .iter()
        .find(|n| n.event == "OnIncomingAgentMessage")
        .expect("complete must emit OnIncomingAgentMessage for Telecom");
    let complete_args_dbg = format!("{:?}", complete_dialogue.args);
    assert!(
        complete_args_dbg.contains("UI/Agents/Dialogue/StandardMissionCompleted"),
        "complete dialogue must use StandardMissionCompleted label: {complete_args_dbg}"
    );
    assert!(
        complete_args_dbg.contains("The Guristas Spies")
            && !complete_args_dbg.contains("agent.missionTemplatizedContent_"),
        "complete dialogue must include readable mission title instead of raw content_template key: {complete_args_dbg}"
    );
    let complete_tx = complete_notifs
        .iter()
        .find(|n| n.event == "OnIncomingTransmission")
        .expect("complete must emit OnIncomingTransmission for Telecom");
    let complete_tx_dbg = format!("{:?}", complete_tx.args);
    assert!(
        complete_tx_dbg.contains("Well done!")
            && !complete_tx_dbg.contains("agent.missionTemplatizedContent_"),
        "complete transmission must emit readable completion prose instead of content_template key: {complete_tx_dbg}"
    );
}

// Regression coverage for #1368 (F-2: Data Center / R&D journal/objective suppression of dungeon warps and R&D completion).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn data_center_and_rnd_objective_and_journal_suppress_dungeons_and_auto_complete() {
    let mut ctx = test_ctx();
    ctx.set_clock(std::sync::Arc::new(
        eve_db::clock::VirtualServerClock::with_fixed_base(1_800_000_000),
    ));
    let char_id = 90_000_001_i64;
    let dc_ag = ctx
        .sde
        .agents
        .values()
        .filter(|a| a.agent_type_id == 5 && a.solar_system_id.is_some() && a.station_id.is_some())
        .min_by_key(|a| a.agent_id)
        .cloned()
        .expect("Data Center agent in SDE");
    let rnd_ag = ctx
        .sde
        .agents
        .values()
        .filter(|a| a.agent_type_id == 4 && a.solar_system_id.is_some() && a.station_id.is_some())
        .min_by_key(|a| a.agent_id)
        .cloned()
        .expect("R&D agent in SDE");

    let mid_dc = 999_901_u32;
    let mid_rnd = 999_902_u32;
    let mut sde = (*ctx.sde).clone();
    let dun_id = sde.dungeons.keys().copied().min();
    sde.missions.insert(
        mid_dc,
        eve_sde::MissionTemplate {
            mission_id: mid_dc,
            title: "Tag Turn-In".into(),
            is_courier: false,
            lp_alpha: 0,
            lp_omega: 0,
            has_standing_rewards: true,
            content_template: Some("agent.missionTemplatizedContent_BasicKillMission".into()),
            dungeon_id: dun_id,
        },
    );
    sde.missions.insert(
        mid_rnd,
        eve_sde::MissionTemplate {
            mission_id: mid_rnd,
            title: "R&D Synthesis".into(),
            is_courier: true,
            lp_alpha: 0,
            lp_omega: 0,
            has_standing_rewards: true,
            content_template: Some("agent.missionTemplatizedContent_Research".into()),
            dungeon_id: dun_id,
        },
    );
    ctx.sde = std::sync::Arc::new(sde);

    let dc_sys = dc_ag.solar_system_id.unwrap();
    let rnd_sys = rnd_ag.solar_system_id.unwrap();
    let now = ctx.now_unix();
    let bm_dc_id = 888_999_101_i64;
    let bm_rnd_id = 888_999_102_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.active_missions.retain(|(cid, _), _| *cid != char_id);
        snap.bookmarks.insert(
            bm_dc_id,
            BookmarkRecord {
                bookmark_id: bm_dc_id,
                owner_id: char_id,
                solar_system_id: dc_sys,
                label: "DC Turn-in".into(),
                x: 100.0,
                y: 200.0,
                z: 300.0,
                item_id: None,
                memo: "Mission Pocket Destination".into(),
                folder_id: None,
                is_shared: false,
            },
        );
        snap.bookmarks.insert(
            bm_rnd_id,
            BookmarkRecord {
                bookmark_id: bm_rnd_id,
                owner_id: char_id,
                solar_system_id: rnd_sys,
                label: "R&D Turn-in".into(),
                x: 400.0,
                y: 500.0,
                z: 600.0,
                item_id: None,
                memo: "Mission Pocket Destination".into(),
                folder_id: None,
                is_shared: false,
            },
        );
        snap.active_missions.insert(
            (char_id, mid_dc),
            make_active_mission(mid_dc, char_id, dc_ag.agent_id, dc_sys, "Accepted", Some(bm_dc_id)),
        );
        let mut dc_meta = make_meta(mid_dc, dc_ag.agent_id, now + 3_600, Some(now + 7_200));
        dc_meta.required_delivery_type_id = Some(17202);
        dc_meta.required_delivery_qty = 1;
        save_mission_meta(snap, char_id, &dc_meta);

        snap.active_missions.insert(
            (char_id, mid_rnd),
            make_active_mission(mid_rnd, char_id, rnd_ag.agent_id, rnd_sys, "Accepted", Some(bm_rnd_id)),
        );
        let mut rnd_meta = make_meta(mid_rnd, rnd_ag.agent_id, now + 3_600, Some(now + 7_200));
        rnd_meta.required_delivery_type_id = Some(34);
        rnd_meta.required_delivery_qty = 300;
        save_mission_meta(snap, char_id, &rnd_meta);
    });

    let mut sess = char_session(char_id, Some(dc_sys), dc_ag.station_id);
    let kw = HashMap::new();
    let journal = ctx
        .dispatch_rpc(
            &mut sess,
            "agentMgr",
            None,
            "GetMyJournalDetails",
            &[],
            &kw,
            Some("machoNet:26000"),
        )
        .await
        .expect("GetMyJournalDetails");
    let PyRep::Tuple(j_parts) = journal else {
        panic!("expected tuple from GetMyJournalDetails");
    };
    let PyRep::List(ref j_missions) = j_parts[0] else {
        panic!("expected missions list");
    };
    for mid in [mid_dc, mid_rnd] {
        let PyRep::Tuple(row) = j_missions
            .iter()
            .find(|r| {
                matches!(r, PyRep::Tuple(cols) if cols.get(9) == Some(&PyRep::Integer(mid as i64)))
            })
            .expect("mission present in journal")
        else {
            panic!("expected 10-tuple mission row");
        };
        assert_eq!(row.len(), 10);
        assert_eq!(
            row.get(2),
            Some(&PyRep::String("UI/Agents/MissionTypes/Trade".into())),
            "#1368 F-2: V24 journal slot [2] must report Trade rather than Encounter"
        );
        let PyRep::List(bms) = &row[6] else {
            panic!("expected bookmarks list");
        };
        assert_eq!(bms.len(), 1);
        assert_eq!(
            bms[0].dict_get("hint"),
            Some(&PyRep::String("UI/Agents/MissionTypes/Trade".into())),
            "Data Center / R&D journal hint must not classify as Encounter or Mining"
        );
        assert_eq!(
            bms[0].dict_get("locationType"),
            Some(&PyRep::String("objective.destination".into()))
        );
        assert_eq!(bms[0].dict_get("deadspace"), Some(&PyRep::Integer(0)));
    }

    let dc_obj = ctx
        .dispatch_rpc(
            &mut sess,
            "agentMgr",
            None,
            "GetMissionObjectiveInfo",
            &[PyRep::Integer(dc_ag.agent_id), PyRep::Integer(mid_dc as i64)],
            &kw,
            Some("machoNet:26000"),
        )
        .await
        .expect("DC GetMissionObjectiveInfo");
    assert_eq!(dc_obj.dict_get("completionStatus"), Some(&PyRep::Integer(0)));
    assert_eq!(dc_obj.dict_get("dungeons"), Some(&PyRep::List(Vec::new())));
    let Some(PyRep::List(dc_objs)) = dc_obj.dict_get("objectives") else {
        panic!("expected DC objectives list");
    };
    assert_eq!(dc_objs.len(), 2);
    let PyRep::Tuple(dc_fetch) = &dc_objs[1] else {
        panic!("expected DC fetch tuple");
    };
    assert_eq!(dc_fetch[0], PyRep::String("fetch".into()));
    let PyRep::Tuple(dc_fetch_args) = &dc_fetch[1] else {
        panic!("expected DC fetch args");
    };
    assert_eq!(dc_fetch_args[2].dict_get("typeID"), Some(&PyRep::Integer(17202)));
    assert_eq!(dc_fetch_args[2].dict_get("quantity"), Some(&PyRep::Integer(1)));
    assert_eq!(dc_fetch_args[2].dict_get("hasCargo"), Some(&PyRep::Bool(false)));
    let dc_expected_vol = ctx.sde.types.get(&17202).map_or(1.0, |t| t.volume);
    let Some(PyRep::Real(dc_vol)) = dc_fetch_args[2].dict_get("volume") else {
        panic!("expected Real volume on DC fetch cargo dict");
    };
    assert!(
        (dc_vol - dc_expected_vol).abs() < 1e-6,
        "DC fetch volume must equal qty * type.volume ({dc_expected_vol}), got {dc_vol}"
    );
    let Some(PyRep::List(dc_locs)) = dc_obj.dict_get("locations") else {
        panic!("expected DC locations");
    };
    assert!(
        dc_locs
            .iter()
            .all(|l| l.dict_get("locationType") != Some(&PyRep::String("dungeon".into()))),
        "DC locations must not emit deadspace dungeon location"
    );

    let mut rnd_sess = char_session(char_id, Some(rnd_sys), rnd_ag.station_id);
    let rnd_obj = ctx
        .dispatch_rpc(
            &mut rnd_sess,
            "agentMgr",
            None,
            "GetMissionObjectiveInfo",
            &[PyRep::Integer(rnd_ag.agent_id), PyRep::Integer(mid_rnd as i64)],
            &kw,
            Some("machoNet:26000"),
        )
        .await
        .expect("R&D GetMissionObjectiveInfo");
    assert_eq!(rnd_obj.dict_get("completionStatus"), Some(&PyRep::Integer(0)));
    assert_eq!(rnd_obj.dict_get("dungeons"), Some(&PyRep::List(Vec::new())));
    let Some(PyRep::List(rnd_objs)) = rnd_obj.dict_get("objectives") else {
        panic!("expected R&D objectives list");
    };
    assert_eq!(rnd_objs.len(), 2);
    let PyRep::Tuple(rnd_fetch) = &rnd_objs[1] else {
        panic!("expected R&D fetch tuple");
    };
    assert_eq!(rnd_fetch[0], PyRep::String("fetch".into()));
    let PyRep::Tuple(rnd_fetch_args) = &rnd_fetch[1] else {
        panic!("expected R&D fetch args");
    };
    assert_eq!(rnd_fetch_args[2].dict_get("typeID"), Some(&PyRep::Integer(34)));
    assert_eq!(rnd_fetch_args[2].dict_get("quantity"), Some(&PyRep::Integer(300)));
    assert_eq!(rnd_fetch_args[2].dict_get("hasCargo"), Some(&PyRep::Bool(false)));
    let trit_unit_vol = ctx.sde.types.get(&34).expect("Tritanium in SDE").volume;
    let rnd_expected_vol = 300.0 * trit_unit_vol;
    let Some(PyRep::Real(rnd_vol)) = rnd_fetch_args[2].dict_get("volume") else {
        panic!("expected Real volume on R&D fetch cargo dict");
    };
    assert!(
        (rnd_vol - rnd_expected_vol).abs() < 1e-6 && (rnd_vol - 3.0).abs() < 1e-6,
        "R&D Tritanium volume must be 300 * 0.01 = 3.0 m3, got {rnd_vol}"
    );
    let Some(PyRep::List(rnd_locs)) = rnd_obj.dict_get("locations") else {
        panic!("expected R&D locations");
    };
    assert!(
        rnd_locs
            .iter()
            .all(|l| l.dict_get("locationType") != Some(&PyRep::String("dungeon".into()))),
        "R&D locations must not emit deadspace dungeon location"
    );

    // #1367: Space encounter dungeon dispatched over machoNet:26000 preserves `stationID: None` + `typeID: 5`.
    let combat_ag = find_standard_agent(&ctx, 1);
    let combat_sys = combat_ag.solar_system_id.unwrap();
    let combat_mid = ctx
        .sde
        .missions
        .values()
        .filter(|m| {
            let ct = m.content_template.as_deref().unwrap_or_default();
            !m.is_courier
                && (ct.contains("BasicKill") || ct.contains("Kill") || ct.contains("Encounter"))
        })
        .min_by_key(|m| m.mission_id)
        .map(|m| m.mission_id)
        .expect("SDE combat mission");
    ctx.db.with_mut_snapshot(|snap| {
        snap.active_missions.insert(
            (char_id, combat_mid),
            make_active_mission(combat_mid, char_id, combat_ag.agent_id, combat_sys, "Accepted", None),
        );
    });
    let mut combat_sess = char_session(char_id, Some(combat_sys), combat_ag.station_id);
    let combat_wire = ctx
        .dispatch_rpc(
            &mut combat_sess,
            "agentMgr",
            None,
            "GetMissionObjectiveInfo",
            &[PyRep::Integer(combat_ag.agent_id), PyRep::Integer(combat_mid as i64)],
            &kw,
            Some("machoNet:26000"),
        )
        .await
        .expect("#1367 combat GetMissionObjectiveInfo");
    let Some(PyRep::List(combat_duns)) = combat_wire.dict_get("dungeons") else {
        panic!("expected combat dungeons list");
    };
    assert_eq!(combat_duns.len(), 1);
    assert_eq!(
        combat_duns[0].dict_get("stationID"),
        Some(&PyRep::None),
        "#1367: deadspace dungeon must preserve stationID == None over MachoNet"
    );
    assert_eq!(combat_duns[0].dict_get("typeID"), Some(&PyRep::Integer(5)));
    assert_eq!(
        combat_duns[0].dict_get("locationID"),
        Some(&PyRep::Integer(i64::from(combat_sys)))
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fetch_objective_has_cargo_eligibility_and_volume_parity() {
    let mut ctx = test_ctx();
    ctx.set_clock(std::sync::Arc::new(
        eve_db::clock::VirtualServerClock::with_fixed_base(1_800_000_000),
    ));
    let char_id = 90_000_001_i64;
    let rnd_ag = ctx
        .sde
        .agents
        .values()
        .filter(|a| a.agent_type_id == 4 && a.solar_system_id.is_some() && a.station_id.is_some())
        .min_by_key(|a| a.agent_id)
        .cloned()
        .expect("R&D agent in SDE");
    let mine_ag = find_standard_agent(&ctx, 1);
    let rnd_sys = rnd_ag.solar_system_id.unwrap();
    let rnd_st = rnd_ag.station_id.unwrap();
    let mine_sys = mine_ag.solar_system_id.unwrap();
    let mine_st = mine_ag.station_id.unwrap();

    let mid_rnd = 999_911_u32;
    let mid_mine = 999_912_u32;
    let mut sde = (*ctx.sde).clone();
    sde.missions.insert(
        mid_rnd,
        eve_sde::MissionTemplate {
            mission_id: mid_rnd,
            title: "R&D Tritanium Delivery".into(),
            is_courier: false,
            lp_alpha: 0,
            lp_omega: 0,
            has_standing_rewards: true,
            content_template: Some("agent.missionTemplatizedContent_Research".into()),
            dungeon_id: None,
        },
    );
    sde.missions.insert(
        mid_mine,
        eve_sde::MissionTemplate {
            mission_id: mid_mine,
            title: "Mine Veldspar".into(),
            is_courier: false,
            lp_alpha: 0,
            lp_omega: 0,
            has_standing_rewards: true,
            content_template: Some("agent.missionTemplatizedContent_BasicMiningMission".into()),
            dungeon_id: None,
        },
    );
    ctx.sde = std::sync::Arc::new(sde);

    let now = ctx.now_unix();
    let ship_id = ctx.db.with_mut_snapshot(|snap| {
        snap.active_missions.retain(|(cid, _), _| *cid != char_id);
        let ship_id = snap
            .characters
            .get(&char_id)
            .map(|c| c.active_ship_id)
            .filter(|&id| id > 0)
            .unwrap_or(100_000_001);
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.active_ship_id = ship_id;
            ch.station_id = Some(rnd_st);
        }
        snap.active_missions.insert(
            (char_id, mid_rnd),
            make_active_mission(mid_rnd, char_id, rnd_ag.agent_id, rnd_sys, "Accepted", None),
        );
        let mut rnd_meta = make_meta(mid_rnd, rnd_ag.agent_id, now + 3_600, Some(now + 7_200));
        rnd_meta.origin_station_id = Some(rnd_st);
        rnd_meta.required_delivery_type_id = Some(34);
        rnd_meta.required_delivery_qty = 300;
        save_mission_meta(snap, char_id, &rnd_meta);

        snap.active_missions.insert(
            (char_id, mid_mine),
            make_active_mission(mid_mine, char_id, mine_ag.agent_id, mine_sys, "Accepted", None),
        );
        let mut mine_meta = make_meta(mid_mine, mine_ag.agent_id, now + 3_600, Some(now + 7_200));
        mine_meta.origin_station_id = Some(mine_st);
        mine_meta.required_delivery_type_id = Some(1230);
        mine_meta.required_delivery_qty = 500;
        save_mission_meta(snap, char_id, &mine_meta);
        ship_id
    });

    let mut sess = char_session(char_id, Some(rnd_sys), Some(rnd_st));
    sess.ship_id = Some(ship_id);
    let kw = HashMap::new();
    let fetch_cargo_dict = |obj: &PyRep| -> PyRep {
        let Some(PyRep::List(objs)) = obj.dict_get("objectives") else {
            panic!("expected objectives list");
        };
        let PyRep::Tuple(entry) = &objs[1] else {
            panic!("expected fetch tuple");
        };
        assert_eq!(entry[0], PyRep::String("fetch".into()));
        let PyRep::Tuple(args) = &entry[1] else {
            panic!("expected fetch args");
        };
        args[2].clone()
    };

    // 1. Accepted with 0 items -> hasCargo == false, volume == 300 * 0.01 = 3.0
    let empty_obj = call_agent_mgr(
        &ctx,
        &mut sess,
        "GetMissionObjectiveInfo",
        &[PyRep::Integer(rnd_ag.agent_id), PyRep::Integer(mid_rnd as i64)],
        &kw,
    )
    .await
    .expect("R&D GetMissionObjectiveInfo empty");
    let empty_dict = fetch_cargo_dict(&empty_obj);
    assert_eq!(empty_dict.dict_get("hasCargo"), Some(&PyRep::Bool(false)));
    let Some(PyRep::Real(empty_vol)) = empty_dict.dict_get("volume") else {
        panic!("expected Real volume");
    };
    assert!((empty_vol - 3.0).abs() < 1e-6, "expected 3.0 m3, got {empty_vol}");

    // 2. Contract-locked, fitted, courier-tagged, and wrong-station stacks must all keep hasCargo == false
    let (id_locked, id_fitted, id_courier, id_wrong_st) = (990_001, 990_002, 990_003, 990_004);
    ctx.db.with_mut_snapshot(|snap| {
        for (iid, loc, flag) in [
            (id_locked, rnd_st, InventoryFlag::Hangar),
            (id_fitted, ship_id, InventoryFlag::HiSlot0),
            (id_courier, rnd_st, InventoryFlag::Hangar),
            (id_wrong_st, rnd_st + 1, InventoryFlag::Hangar),
        ] {
            snap.items.insert(
                iid,
                InventoryItemRecord {
                    item_id: iid,
                    type_id: 34,
                    owner_id: char_id,
                    location_id: loc,
                    flag,
                    quantity: 300,
                    singleton: false,
                    ..Default::default()
                },
            );
        }
        snap.contracts.insert(
            990_901,
            eve_db::ContractRecord {
                contract_id: 990_901,
                status: "Outstanding".into(),
                item_ids: vec![id_locked],
                ..Default::default()
            },
        );
        snap.item_custom_names
            .insert(id_courier, format!("courier_cargo:{char_id}:777"));
    });
    let ineligible_obj = call_agent_mgr(
        &ctx,
        &mut sess,
        "GetMissionObjectiveInfo",
        &[PyRep::Integer(rnd_ag.agent_id), PyRep::Integer(mid_rnd as i64)],
        &kw,
    )
    .await
    .expect("R&D GetMissionObjectiveInfo ineligible");
    assert_eq!(
        fetch_cargo_dict(&ineligible_obj).dict_get("hasCargo"),
        Some(&PyRep::Bool(false)),
        "contract-locked, fitted, courier-tagged, and wrong-station items must not set hasCargo"
    );

    // 3. 299 eligible units (< 300) still false; adding 1 more eligible unit in ship Cargo flips true
    let (id_hangar, id_cargo) = (990_005, 990_006);
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            id_hangar,
            InventoryItemRecord {
                item_id: id_hangar,
                type_id: 34,
                owner_id: char_id,
                location_id: rnd_st,
                flag: InventoryFlag::Hangar,
                quantity: 299,
                singleton: false,
                ..Default::default()
            },
        );
    });
    let partial_obj = call_agent_mgr(
        &ctx,
        &mut sess,
        "GetMissionObjectiveInfo",
        &[PyRep::Integer(rnd_ag.agent_id), PyRep::Integer(mid_rnd as i64)],
        &kw,
    )
    .await
    .expect("R&D GetMissionObjectiveInfo partial");
    assert_eq!(
        fetch_cargo_dict(&partial_obj).dict_get("hasCargo"),
        Some(&PyRep::Bool(false))
    );

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            id_cargo,
            InventoryItemRecord {
                item_id: id_cargo,
                type_id: 34,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                singleton: false,
                ..Default::default()
            },
        );
    });
    let ready_obj = call_agent_mgr(
        &ctx,
        &mut sess,
        "GetMissionObjectiveInfo",
        &[PyRep::Integer(rnd_ag.agent_id), PyRep::Integer(mid_rnd as i64)],
        &kw,
    )
    .await
    .expect("R&D GetMissionObjectiveInfo ready");
    assert_eq!(
        fetch_cargo_dict(&ready_obj).dict_get("hasCargo"),
        Some(&PyRep::Bool(true))
    );

    // 4. Turn-in succeeds using the exact same eligible items, leaving locked/fitted/courier/wrong-st intact;
    // Completed mission still reports hasCargo == true via completion_status > 0
    let done = call_agent_mgr(
        &ctx,
        &mut sess,
        "DoAction",
        &[
            PyRep::Integer(6),
            PyRep::Integer(rnd_ag.agent_id),
            PyRep::Integer(mid_rnd as i64),
        ],
        &kw,
    )
    .await
    .expect("R&D DoAction(6) complete");
    assert_eq!(done.dict_get("state"), Some(&PyRep::String("Completed".into())));
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.items.contains_key(&id_hangar));
        assert!(!snap.items.contains_key(&id_cargo));
        for iid in [id_locked, id_fitted, id_courier, id_wrong_st] {
            assert_eq!(snap.items.get(&iid).map(|i| i.quantity), Some(300));
        }
    });
    let completed_obj = call_agent_mgr(
        &ctx,
        &mut sess,
        "GetMissionObjectiveInfo",
        &[PyRep::Integer(rnd_ag.agent_id), PyRep::Integer(mid_rnd as i64)],
        &kw,
    )
    .await
    .expect("R&D GetMissionObjectiveInfo completed");
    assert_eq!(
        fetch_cargo_dict(&completed_obj).dict_get("hasCargo"),
        Some(&PyRep::Bool(true))
    );

    // 5. Mining fetch checks SpecializedOreHold eligibility and Veldspar volume (500 * 0.1 = 50.0 m3)
    let mut sess_mine = char_session(char_id, Some(mine_sys), Some(mine_st));
    sess_mine.ship_id = Some(ship_id);
    let mine_empty = call_agent_mgr(
        &ctx,
        &mut sess_mine,
        "GetMissionObjectiveInfo",
        &[PyRep::Integer(mine_ag.agent_id), PyRep::Integer(mid_mine as i64)],
        &kw,
    )
    .await
    .expect("Mining GetMissionObjectiveInfo empty");
    let mine_empty_dict = fetch_cargo_dict(&mine_empty);
    assert_eq!(mine_empty_dict.dict_get("hasCargo"), Some(&PyRep::Bool(false)));
    let Some(PyRep::Real(mine_vol)) = mine_empty_dict.dict_get("volume") else {
        panic!("expected Real volume on mining objective");
    };
    let veldspar_unit_vol = ctx.sde.types.get(&1230).expect("Veldspar in SDE").volume;
    assert!(
        (mine_vol - 500.0 * veldspar_unit_vol).abs() < 1e-6 && (mine_vol - 50.0).abs() < 1e-6,
        "Mining Veldspar volume must be 500 * 0.1 = 50.0 m3, got {mine_vol}"
    );

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            990_007,
            InventoryItemRecord {
                item_id: 990_007,
                type_id: 1230,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::SpecializedOreHold,
                quantity: 500,
                singleton: false,
                ..Default::default()
            },
        );
    });
    let mine_ready = call_agent_mgr(
        &ctx,
        &mut sess_mine,
        "GetMissionObjectiveInfo",
        &[PyRep::Integer(mine_ag.agent_id), PyRep::Integer(mid_mine as i64)],
        &kw,
    )
    .await
    .expect("Mining GetMissionObjectiveInfo ore hold ready");
    assert_eq!(
        fetch_cargo_dict(&mine_ready).dict_get("hasCargo"),
        Some(&PyRep::Bool(true))
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn do_action_expiry_cleans_bookmarks_and_rnd_courier_uses_archetype() {
    let mut ctx = test_ctx();
    ctx.set_clock(std::sync::Arc::new(
        eve_db::clock::VirtualServerClock::with_fixed_base(1_800_000_000),
    ));
    let char_id = 90_000_001_i64;
    let std_ag = find_standard_agent(&ctx, 1);
    let std_sys = std_ag.solar_system_id.unwrap();
    let std_st = std_ag.station_id.unwrap();
    let rnd_ag = ctx
        .sde
        .agents
        .values()
        .filter(|a| a.agent_type_id == 4 && a.solar_system_id.is_some() && a.station_id.is_some())
        .min_by_key(|a| a.agent_id)
        .cloned()
        .expect("R&D agent in SDE");
    let rnd_sys = rnd_ag.solar_system_id.unwrap();
    let rnd_st = rnd_ag.station_id.unwrap();
    let dest_st = ctx
        .sde
        .stations
        .keys()
        .copied()
        .filter(|&s| s != rnd_st)
        .min()
        .unwrap_or(rnd_st);
    let dest_sys = ctx
        .sde
        .stations
        .get(&dest_st)
        .map_or(rnd_sys, |s| s.solar_system_id);

    let (mid_offer, mid_accept, mid_rnd_courier) = (999_921_u32, 999_922_u32, 999_923_u32);
    let mut sde = (*ctx.sde).clone();
    for (mid, title, is_courier, ct) in [
        (mid_offer, "Expiring Offer", false, "agent.missionTemplatizedContent_BasicKillMission"),
        (mid_accept, "Expiring Accept", false, "agent.missionTemplatizedContent_BasicKillMission"),
        (mid_rnd_courier, "R&D Courier", true, "agent.missionTemplatizedContent_BasicCourierMission"),
    ] {
        sde.missions.insert(
            mid,
            eve_sde::MissionTemplate {
                mission_id: mid,
                title: title.into(),
                is_courier,
                lp_alpha: 0,
                lp_omega: 0,
                has_standing_rewards: true,
                content_template: Some(ct.into()),
                dungeon_id: None,
            },
        );
    }
    ctx.sde = std::sync::Arc::new(sde);

    let now = ctx.now_unix();
    let (bm_offer, bm_accept) = (888_999_201_i64, 888_999_202_i64);
    ctx.db.with_mut_snapshot(|snap| {
        snap.active_missions.retain(|(cid, _), _| *cid != char_id);
        for (bmid, label) in [(bm_offer, "Offer BM"), (bm_accept, "Accept BM")] {
            snap.bookmarks.insert(
                bmid,
                BookmarkRecord {
                    bookmark_id: bmid,
                    owner_id: char_id,
                    solar_system_id: std_sys,
                    label: label.into(),
                    x: 10.0,
                    y: 20.0,
                    z: 30.0,
                    item_id: None,
                    memo: "Mission Pocket Destination".into(),
                    folder_id: None,
                    is_shared: false,
                },
            );
        }
        snap.active_missions.insert(
            (char_id, mid_offer),
            make_active_mission(mid_offer, char_id, std_ag.agent_id, std_sys, "Offered", Some(bm_offer)),
        );
        save_mission_meta(
            snap,
            char_id,
            &make_meta(mid_offer, std_ag.agent_id, now.saturating_sub(10), None),
        );

        snap.active_missions.insert(
            (char_id, mid_accept),
            make_active_mission(mid_accept, char_id, std_ag.agent_id, std_sys, "Accepted", Some(bm_accept)),
        );
        save_mission_meta(
            snap,
            char_id,
            &make_meta(mid_accept, std_ag.agent_id, now + 1000, Some(now.saturating_sub(5))),
        );

        snap.active_missions.insert(
            (char_id, mid_rnd_courier),
            make_active_mission(mid_rnd_courier, char_id, rnd_ag.agent_id, rnd_sys, "Accepted", None),
        );
        let mut rnd_courier_meta =
            make_meta(mid_rnd_courier, rnd_ag.agent_id, now + 3_600, Some(now + 7_200));
        rnd_courier_meta.origin_station_id = Some(rnd_st);
        rnd_courier_meta.destination_station_id = Some(dest_st);
        save_mission_meta(snap, char_id, &rnd_courier_meta);
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = Some(dest_st);
        }
    });

    let mut sess_std = char_session(char_id, Some(std_sys), Some(std_st));
    let kw = HashMap::new();

    // P2-1: DoAction Offer expiry clears stale bookmark_id and removes bookmark
    let offer_err = call_agent_mgr(
        &ctx,
        &mut sess_std,
        "DoAction",
        &[
            PyRep::Integer(3),
            PyRep::Integer(std_ag.agent_id),
            PyRep::Integer(mid_offer as i64),
        ],
        &kw,
    )
    .await
    .unwrap_err();
    assert_eq!(offer_err.code, "MissionOfferExpired");

    // P2-1: DoAction Accept expiry clears stale bookmark_id and removes bookmark
    let accept_err = call_agent_mgr(
        &ctx,
        &mut sess_std,
        "DoAction",
        &[
            PyRep::Integer(6),
            PyRep::Integer(std_ag.agent_id),
            PyRep::Integer(mid_accept as i64),
        ],
        &kw,
    )
    .await
    .unwrap_err();
    assert_eq!(accept_err.code, "MissionExpired");

    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.active_missions[&(char_id, mid_offer)].state, "Expired");
        assert_eq!(snap.active_missions[&(char_id, mid_offer)].bookmark_id, None);
        assert!(!snap.bookmarks.contains_key(&bm_offer));

        assert_eq!(snap.active_missions[&(char_id, mid_accept)].state, "Failed");
        assert_eq!(snap.active_missions[&(char_id, mid_accept)].npcs_remaining, 0);
        assert_eq!(snap.active_missions[&(char_id, mid_accept)].bookmark_id, None);
        assert!(!snap.bookmarks.contains_key(&bm_accept));
    });

    // P2-2: R&D agent courier mission uses arch.is_rnd_turnin == false (does not force Tritanium turn-in)
    let mut sess_rnd = char_session(char_id, Some(dest_sys), Some(dest_st));
    let rnd_courier_obj = call_agent_mgr(
        &ctx,
        &mut sess_rnd,
        "GetMissionObjectiveInfo",
        &[
            PyRep::Integer(rnd_ag.agent_id),
            PyRep::Integer(mid_rnd_courier as i64),
        ],
        &kw,
    )
    .await
    .expect("R&D courier GetMissionObjectiveInfo");
    let Some(PyRep::List(objs)) = rnd_courier_obj.dict_get("objectives") else {
        panic!("expected objectives");
    };
    let PyRep::Tuple(second_obj) = &objs[1] else {
        panic!("expected second objective tuple");
    };
    assert_eq!(second_obj[0], PyRep::String("transport".into()));

    let done = call_agent_mgr(
        &ctx,
        &mut sess_rnd,
        "DoAction",
        &[
            PyRep::Integer(6),
            PyRep::Integer(rnd_ag.agent_id),
            PyRep::Integer(mid_rnd_courier as i64),
        ],
        &kw,
    )
    .await
    .expect("R&D courier DoAction(6) must not require Tritanium turn-in");
    assert_eq!(done.dict_get("state"), Some(&PyRep::String("Completed".into())));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fetch_objective_and_completion_exclude_boarded_active_ship() {
    let mut ctx = test_ctx();
    ctx.set_clock(std::sync::Arc::new(
        eve_db::clock::VirtualServerClock::with_fixed_base(1_800_000_000),
    ));
    let char_id = 90_000_001_i64;
    let ag = find_standard_agent(&ctx, 1);
    let sys_id = ag.solar_system_id.unwrap();
    let st_id = ag.station_id.unwrap();
    let mid_ship = 999_931_u32;
    let mut sde = (*ctx.sde).clone();
    sde.missions.insert(
        mid_ship,
        eve_sde::MissionTemplate {
            mission_id: mid_ship,
            title: "Deliver Velator Hull".into(),
            is_courier: false,
            lp_alpha: 0,
            lp_omega: 0,
            has_standing_rewards: true,
            content_template: Some("agent.missionTemplatizedContent_BasicTradeMission".into()),
            dungeon_id: None,
        },
    );
    ctx.sde = std::sync::Arc::new(sde);

    let now = ctx.now_unix();
    let ship_id = ctx.db.with_mut_snapshot(|snap| {
        snap.active_missions.retain(|(cid, _), _| *cid != char_id);
        let ship_id = snap
            .characters
            .get(&char_id)
            .map(|c| c.active_ship_id)
            .filter(|&id| id > 0)
            .unwrap_or(100_000_001);
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.active_ship_id = ship_id;
            ch.station_id = Some(st_id);
        }
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 606,
                owner_id: char_id,
                location_id: st_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.active_missions.insert(
            (char_id, mid_ship),
            make_active_mission(mid_ship, char_id, ag.agent_id, sys_id, "Accepted", None),
        );
        let mut meta = make_meta(mid_ship, ag.agent_id, now + 3_600, Some(now + 7_200));
        meta.origin_station_id = Some(st_id);
        meta.required_delivery_type_id = Some(606);
        meta.required_delivery_qty = 1;
        save_mission_meta(snap, char_id, &meta);
        ship_id
    });

    let mut sess = char_session(char_id, Some(sys_id), Some(st_id));
    sess.ship_id = Some(ship_id);
    let kw = HashMap::new();
    let fetch_has_cargo = |obj: &PyRep| -> Option<PyRep> {
        let Some(PyRep::List(objs)) = obj.dict_get("objectives") else {
            return None;
        };
        let PyRep::Tuple(entry) = &objs[1] else {
            return None;
        };
        let PyRep::Tuple(args) = &entry[1] else {
            return None;
        };
        args[2].dict_get("hasCargo").cloned()
    };

    // Boarded active_ship with no spare hull -> hasCargo == false and DoAction(6) fails closed
    let obj_one = call_agent_mgr(
        &ctx,
        &mut sess,
        "GetMissionObjectiveInfo",
        &[PyRep::Integer(ag.agent_id), PyRep::Integer(mid_ship as i64)],
        &kw,
    )
    .await
    .expect("GetMissionObjectiveInfo with boarded ship only");
    assert_eq!(fetch_has_cargo(&obj_one), Some(PyRep::Bool(false)));
    let err_one = call_agent_mgr(
        &ctx,
        &mut sess,
        "DoAction",
        &[PyRep::Integer(6), PyRep::Integer(ag.agent_id), PyRep::Integer(mid_ship as i64)],
        &kw,
    )
    .await
    .unwrap_err();
    assert_eq!(err_one.code, "MissionIncomplete");

    // req_units == 2 with boarded active_ship + 1 spare hull -> still hasCargo == false & fails closed
    let spare_ship = 990_102_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            spare_ship,
            InventoryItemRecord {
                item_id: spare_ship,
                type_id: 606,
                owner_id: char_id,
                location_id: st_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        let mut meta = make_meta(mid_ship, ag.agent_id, now + 3_600, Some(now + 7_200));
        meta.origin_station_id = Some(st_id);
        meta.required_delivery_type_id = Some(606);
        meta.required_delivery_qty = 2;
        save_mission_meta(snap, char_id, &meta);
    });
    let obj_two = call_agent_mgr(
        &ctx,
        &mut sess,
        "GetMissionObjectiveInfo",
        &[PyRep::Integer(ag.agent_id), PyRep::Integer(mid_ship as i64)],
        &kw,
    )
    .await
    .expect("GetMissionObjectiveInfo with boarded ship + 1 spare hull for qty=2");
    assert_eq!(fetch_has_cargo(&obj_two), Some(PyRep::Bool(false)));
    let err_two = call_agent_mgr(
        &ctx,
        &mut sess,
        "DoAction",
        &[PyRep::Integer(6), PyRep::Integer(ag.agent_id), PyRep::Integer(mid_ship as i64)],
        &kw,
    )
    .await
    .unwrap_err();
    assert_eq!(err_two.code, "MissionIncomplete");
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.active_missions[&(char_id, mid_ship)].state, "Accepted");
        assert!(snap.items.contains_key(&ship_id));
        assert!(snap.items.contains_key(&spare_ship));
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn in_space_agent_excludes_acl_denied_structure_hangar_delivery() {
    let mut ctx = test_ctx();
    ctx.set_clock(std::sync::Arc::new(
        eve_db::clock::VirtualServerClock::with_fixed_base(1_800_000_000),
    ));
    let char_id = 90_000_001_i64;
    let mut dc_ag = ctx
        .sde
        .agents
        .values()
        .filter(|a| a.agent_type_id == 5 && a.solar_system_id.is_some())
        .min_by_key(|a| a.agent_id)
        .cloned()
        .expect("Data Center agent in SDE");
    dc_ag.station_id = None;
    let dc_sys = dc_ag.solar_system_id.unwrap();
    let mid_inspace = 999_932_u32;
    let mut sde = (*ctx.sde).clone();
    sde.agents.insert(dc_ag.agent_id, dc_ag.clone());
    sde.missions.insert(
        mid_inspace,
        eve_sde::MissionTemplate {
            mission_id: mid_inspace,
            title: "In-Space Tag Turn-In".into(),
            is_courier: false,
            lp_alpha: 0,
            lp_omega: 0,
            has_standing_rewards: true,
            content_template: Some("agent.missionTemplatizedContent_BasicKillMission".into()),
            dungeon_id: None,
        },
    );
    ctx.sde = std::sync::Arc::new(sde);

    let now = ctx.now_unix();
    let struct_id = 1_000_000_777_i64;
    let tag_id = 990_201_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.active_missions.retain(|(cid, _), _| *cid != char_id);
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = None;
        }
        snap.items.insert(
            tag_id,
            InventoryItemRecord {
                item_id: tag_id,
                type_id: 17202,
                owner_id: char_id,
                location_id: struct_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: false,
                ..Default::default()
            },
        );
        snap.structure_docking_acl_denied
            .entry(struct_id)
            .or_default()
            .insert(char_id);
        snap.active_missions.insert(
            (char_id, mid_inspace),
            make_active_mission(mid_inspace, char_id, dc_ag.agent_id, dc_sys, "Accepted", None),
        );
        let mut meta = make_meta(mid_inspace, dc_ag.agent_id, now + 3_600, Some(now + 7_200));
        meta.origin_station_id = None;
        meta.required_delivery_type_id = Some(17202);
        meta.required_delivery_qty = 1;
        save_mission_meta(snap, char_id, &meta);
    });

    let mut sess = char_session(char_id, Some(dc_sys), None);
    let kw = HashMap::new();
    let fetch_has_cargo = |obj: &PyRep| -> Option<PyRep> {
        let Some(PyRep::List(objs)) = obj.dict_get("objectives") else {
            return None;
        };
        let PyRep::Tuple(entry) = &objs[1] else {
            return None;
        };
        let PyRep::Tuple(args) = &entry[1] else {
            return None;
        };
        args[2].dict_get("hasCargo").cloned()
    };

    let denied_obj = call_agent_mgr(
        &ctx,
        &mut sess,
        "GetMissionObjectiveInfo",
        &[PyRep::Integer(dc_ag.agent_id), PyRep::Integer(mid_inspace as i64)],
        &kw,
    )
    .await
    .expect("in-space GetMissionObjectiveInfo ACL denied");
    assert_eq!(fetch_has_cargo(&denied_obj), Some(PyRep::Bool(false)));
    let denied_err = call_agent_mgr(
        &ctx,
        &mut sess,
        "DoAction",
        &[PyRep::Integer(6), PyRep::Integer(dc_ag.agent_id), PyRep::Integer(mid_inspace as i64)],
        &kw,
    )
    .await
    .unwrap_err();
    assert_eq!(denied_err.code, "MissionIncomplete");
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.active_missions[&(char_id, mid_inspace)].state, "Accepted");
        assert_eq!(snap.items.get(&tag_id).map(|i| i.quantity), Some(1));
    });

    // Clearing structure ACL denial makes the item eligible for both hasCargo and completion
    ctx.db.with_mut_snapshot(|snap| {
        snap.structure_docking_acl_denied.remove(&struct_id);
    });
    let allowed_obj = call_agent_mgr(
        &ctx,
        &mut sess,
        "GetMissionObjectiveInfo",
        &[PyRep::Integer(dc_ag.agent_id), PyRep::Integer(mid_inspace as i64)],
        &kw,
    )
    .await
    .expect("in-space GetMissionObjectiveInfo ACL allowed");
    assert_eq!(fetch_has_cargo(&allowed_obj), Some(PyRep::Bool(true)));
    let done = call_agent_mgr(
        &ctx,
        &mut sess,
        "DoAction",
        &[PyRep::Integer(6), PyRep::Integer(dc_ag.agent_id), PyRep::Integer(mid_inspace as i64)],
        &kw,
    )
    .await
    .expect("in-space DoAction(6) completes once ACL allowed");
    assert_eq!(done.dict_get("state"), Some(&PyRep::String("Completed".into())));
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.items.contains_key(&tag_id));
    });
}
