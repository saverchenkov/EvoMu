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
    assert_eq!(missions[0].dict_get("missionID"), Some(&PyRep::Integer(mid_active as i64)));
    assert_eq!(missions[0].dict_get("level"), Some(&PyRep::Integer(3)));
    assert_eq!(missions[0].dict_get("missionState"), Some(&PyRep::Integer(2)));

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
    let Some(PyRep::String(obj_str)) = obj_info.dict_get("objectives") else { panic!("expected objectives") };
    assert!(obj_str.contains("Transport 250 units"));
    assert_eq!(obj_info.dict_get("requiredTypeID"), Some(&PyRep::Integer(34)));
    assert_eq!(obj_info.dict_get("requiredQuantity"), Some(&PyRep::Integer(250)));

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
            t.is_courier
                && !t.content_template.as_deref().unwrap_or_default().contains("Trade")
        })
        .min_by_key(|&(&id, _)| id)
        .or_else(|| {
            ctx.sde
                .missions
                .iter()
                .filter(|(_, t)| !t.content_template.as_deref().unwrap_or_default().contains("Trade"))
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
        .unwrap_err()
        .code,
        "MissionNotActive"
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
            .unwrap_err()
            .code,
            "MissionNotActive",
            "unbound GetMissionObjectiveInfo(agent_id) must reject {state} missions"
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

    // Pick a real non-courier mission (for #998 bookmark + #999 dungeon ownerID) and a courier mission (for #1000).
    let encounter_mid = ctx
        .sde
        .missions
        .values()
        .find(|m| !m.is_courier && !m.content_template.as_deref().unwrap_or_default().contains("Trade"))
        .map(|m| m.mission_id)
        .unwrap_or(10_301);
    let courier_mid = ctx
        .sde
        .missions
        .values()
        .find(|m| m.is_courier && !m.content_template.as_deref().unwrap_or_default().contains("Trade"))
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
    let enc_row = j_missions
        .iter()
        .find(|r| r.dict_get("missionID") == Some(&PyRep::Integer(encounter_mid as i64)))
        .expect("encounter mission in journal");
    let Some(PyRep::List(bms)) = enc_row.dict_get("bookmarks") else {
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
        let enc_row2 = j2_missions
            .iter()
            .find(|r| r.dict_get("missionID") == Some(&PyRep::Integer(encounter_mid as i64)))
            .expect("encounter row2");
        let Some(PyRep::List(bms2)) = enc_row2.dict_get("bookmarks") else {
            panic!("expected bookmarks list2");
        };
        let st_bm = bms2
            .iter()
            .find(|b| b.dict_get("bookmarkID") == Some(&PyRep::Integer(st_bm_id)))
            .expect("station bookmark");
        assert_eq!(st_bm.dict_get("itemID"), Some(&PyRep::Integer(st_id)));
        assert_eq!(st_bm.dict_get("typeID"), Some(&PyRep::Integer(st_type_id)));
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
    assert_eq!(b1_missions[0].dict_get("agentID"), Some(&PyRep::Integer(ag1_id)));
    assert_eq!(b1_missions[0].dict_get("missionID"), Some(&PyRep::Integer(mid1 as i64)));
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
    assert_eq!(b2_missions[0].dict_get("agentID"), Some(&PyRep::Integer(ag2_id)));
    assert_eq!(b2_missions[0].dict_get("missionID"), Some(&PyRep::Integer(mid2 as i64)));
    assert_eq!(b2_research.len(), 1);
    assert_eq!(b2_research[0].dict_get("agentID"), Some(&PyRep::Integer(ag2_id)));
}
