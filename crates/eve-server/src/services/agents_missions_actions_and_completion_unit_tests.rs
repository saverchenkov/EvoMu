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
        .find(|a| {
            a.agent_type_id == 9
                && a.solar_system_id.is_some()
                && ctx.sde.factions.values().any(|f| f.militia_corporation_id == Some(a.corporation_id))
        })
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
        .find(|a| matches!(a.agent_type_id, 5 | 8 | 12) && a.solar_system_id.is_some())
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
        .find(|(_, t)| {
            t.is_courier
                && !t.content_template.as_deref().unwrap_or_default().contains("Trade")
        })
        .or_else(|| {
            ctx.sde
                .missions
                .iter()
                .find(|(_, t)| !t.content_template.as_deref().unwrap_or_default().contains("Trade"))
        })
        .map(|(&id, _)| id)
        .unwrap_or_else(|| *ctx.sde.missions.keys().next().expect("SDE mission"));

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
    let dest_st = *ctx.sde.stations.keys().find(|&&k| k != st_id).unwrap_or(&(st_id + 1));
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
}
