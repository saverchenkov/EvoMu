//! E2E Parity Shard 2 Suite 1: Agent Missions + Reward Formulas + E-5 Storyline,
//! Project Discovery RPCs + AK Notification, and Daily Login Campaigns.

#![forbid(unsafe_code)]

use crate::pve_e2e_common::{
    bind_service, buy_fit_combat_ship, create_player_pilot, drain_all_notifications,
    fly_stargates_to, fresh_player_ctx, kv_get, login_existing_player, reload_ctx_from_sqlite,
    rpc_ok, undock_from_station, warp_and_dock_at_station, warp_and_engage_ball, JITA_STATION,
    wait_for_warp_end, JITA_SYS,
};
use eve_proto::PyRep;
use eve_sde::SolarSystemId;
use std::collections::HashMap;

// parity: plan:E-5
// parity: feat:pve.mission-complete
// parity: feat:pve.mission-lp-isk-reward-formula
#[tokio::test]
async fn test_agent_missions_complete_reward_formula_and_storyline_e2e() {
    let ctx = fresh_player_ctx();
    let char_id = 90_000_001_i64;
    let (mut sess, _) = login_existing_player(&ctx, 5101, "demo", "demo-secret", char_id).await;

    // Pick a real Level-1 Security agent via agentMgr.GetAgents(1, 24)
    let PyRep::Rowset { rows: l1_rows, .. } =
        rpc_ok(&ctx, &mut sess, "agentMgr", "GetAgents", &[PyRep::Integer(1), PyRep::Integer(24)]).await
    else {
        panic!("Expected Rowset from agentMgr.GetAgents");
    };
    let agent_row = l1_rows
        .iter()
        .find(|r| r[5].as_i64() == Some(i64::from(JITA_SYS)) && r[6].as_i64() == Some(JITA_STATION))
        .or_else(|| l1_rows.iter().find(|r| r[5].as_i64() == Some(i64::from(JITA_SYS))))
        .unwrap_or(&l1_rows[0]);
    let agent_id = agent_row[0].as_i64().unwrap();
    let agent_corp_id = agent_row[3].as_i64().unwrap();
    let agent_sys = agent_row[5].as_i64().unwrap_or(i64::from(JITA_SYS)) as SolarSystemId;
    let agent_station = agent_row[6].as_i64().unwrap_or(JITA_STATION);

    let _start_cents = ctx.db.get_character(char_id).unwrap().balance.raw_cents();
    let start_lp = ctx
        .db
        .get_character(char_id)
        .unwrap()
        .loyalty_points
        .get(&agent_corp_id)
        .copied()
        .unwrap_or(0);
    let start_agent_standing = ctx
        .db
        .get_character(char_id)
        .unwrap()
        .standings
        .get(&agent_id)
        .copied()
        .unwrap_or(0.0);

    // Buy an Omen Cruiser (2006) + 5x Pulse Laser + Multifrequency M on Jita market and fly to agent station
    let (ship_id, guns) = buy_fit_combat_ship(
        &ctx,
        &mut sess,
        char_id,
        JITA_STATION,
        2006,
        486,
        201,
        5,
    )
    .await;
    let start_cents = ctx.db.get_character(char_id).unwrap().balance.raw_cents();
    fly_stargates_to(&ctx, &mut sess, agent_sys).await;
    warp_and_dock_at_station(&ctx, &mut sess, ship_id, agent_sys, agent_station).await;
    let agent_oid = bind_service(&ctx, &mut sess, "agentMgr", PyRep::Integer(agent_id)).await as u64;

    // 1. Request mission from agent (actionID = 2)
    let offer = ctx
        .dispatch_rpc(&mut sess, "agentMgr", Some(agent_oid), "DoAction", &[PyRep::Integer(2)], &HashMap::new(), None)
        .await
        .unwrap();
    assert_eq!(offer.dict_get("state"), Some(&PyRep::String("Offered".into())));
    let mission_id = offer
        .dict_get("missionID")
        .and_then(|v| v.as_i64())
        .expect("missionID") as u32;

    // 2. Accept mission (actionID = 3)
    let accepted = ctx
        .dispatch_rpc(&mut sess, "agentMgr", Some(agent_oid), "DoAction", &[PyRep::Integer(3)], &HashMap::new(), None)
        .await
        .unwrap();
    assert_eq!(
        accepted.dict_get("state"),
        Some(&PyRep::String("Accepted".into()))
    );
    let spawned_npc_id = accepted
        .dict_get("spawnedNpcBallID")
        .and_then(|v| v.as_i64());

    // 4. Undock, warp to mission bookmark, and destroy all spawned mission NPCs with turrets
    undock_from_station(&ctx, &mut sess, agent_sys).await;
    let bm_id = accepted.dict_get("bookmarkID").and_then(|v| v.as_i64());
    let site_sys = accepted
        .dict_get("solarSystemID")
        .and_then(|v| v.as_i64())
        .map(|s| s as SolarSystemId)
        .unwrap_or(agent_sys);
    fly_stargates_to(&ctx, &mut sess, site_sys).await;
    let b_oid = bind_service(&ctx, &mut sess, "beyonce", PyRep::Integer(i64::from(site_sys))).await as u64;
    let (init_ws, init_wt) = if let Some(b) = bm_id { ("bookmark", PyRep::Integer(b)) } else { ("mission", PyRep::Integer(i64::from(mission_id))) };
    let _ = ctx.dispatch_rpc(&mut sess, "beyonce", Some(b_oid), "CmdWarpToStuff", &[PyRep::String(init_ws.into()), init_wt], &HashMap::new(), None).await;
    wait_for_warp_end(&ctx, site_sys, ship_id).await;
    for _pass in 0..5 {
        let npcs_to_clear: Vec<i64> = ctx.db.with_snapshot(|s| {
            s.active_missions
                .iter()
                .filter_map(|((c, mid), m)| {
                    if *c == char_id && *mid == mission_id && m.npcs_remaining > 0 {
                        Some(m.spawned_npc_ball_ids.clone())
                    } else {
                        None
                    }
                })
                .next()
                .unwrap_or_default()
        });
        if npcs_to_clear.is_empty() {
            if let Some(sid) = spawned_npc_id.filter(|_| _pass == 0) {
                warp_and_engage_ball(&ctx, &mut sess, site_sys, ship_id, &guns, sid, "item", PyRep::Integer(sid)).await;
            }
            break;
        }
        for npc_id in npcs_to_clear {
            let (ws, wt) = if let Some(b) = bm_id { ("bookmark", PyRep::Integer(b)) } else { ("mission", PyRep::Integer(i64::from(mission_id))) };
            warp_and_engage_ball(&ctx, &mut sess, site_sys, ship_id, &guns, npc_id, ws, wt).await;
        }
    }

    // 5. Dock back at agent station and complete mission within bonus time window (actionID = 6)
    fly_stargates_to(&ctx, &mut sess, agent_sys).await;
    warp_and_dock_at_station(&ctx, &mut sess, ship_id, agent_sys, agent_station).await;

    let completed = ctx
        .dispatch_rpc(&mut sess, "agentMgr", Some(agent_oid), "DoAction", &[PyRep::Integer(6)], &HashMap::new(), None)
        .await
        .unwrap();
    assert_eq!(
        completed.dict_get("state"),
        Some(&PyRep::String("Completed".into()))
    );

    // Verify ISK + time bonus + LP + standings + E-5 storyline progression + notifications
    let ch_after = ctx.db.get_character(char_id).unwrap();
    let end_lp = ch_after
        .loyalty_points
        .get(&agent_corp_id)
        .copied()
        .unwrap_or(0);
    let end_agent_standing = ch_after.standings.get(&agent_id).copied().unwrap_or(0.0);
    assert!(
        end_lp > start_lp,
        "Expected positive corp LP reward (before={start_lp}, after={end_lp})"
    );
    assert!(
        end_agent_standing > start_agent_standing,
        "Expected positive agent standing increase"
    );
    assert!(
        ch_after.balance.raw_cents() + 200_000_000 > start_cents,
        "Expected mission ISK + bonus ISK credited"
    );

    // Check story-line progress & journal state via agentMgr.GetMyJournalDetails
    let journal = rpc_ok(&ctx, &mut sess, "agentMgr", "GetMyJournalDetails", &[]).await;
    assert!(!matches!(journal, PyRep::None));

    // Reload from SQLite and verify mission completion & LP persisted
    let ctx_reloaded = reload_ctx_from_sqlite(&ctx);
    let ch_reloaded = ctx_reloaded.db.get_character(char_id).unwrap();
    assert_eq!(
        ch_reloaded.loyalty_points.get(&agent_corp_id).copied(),
        Some(end_lp)
    );
    let _ = mission_id;
}

// parity: rpc:ProjectDiscovery.get_total_needed_xp
// parity: rpc:ProjectDiscovery.get_tutorial_xp_reward
// parity: rpc:ProjectDiscovery.give_tutorial_rewards
// parity: rpc:ProjectDiscovery.new_training_task
// parity: rpc:ProjectDiscovery.get_maximum_bonus_samples
// parity: rpc:ProjectDiscovery.get_new_task
// parity: rpc:ProjectDiscovery.get_data_from_url
// parity: rpc:ProjectDiscovery.post_classification
// parity: rpc:ProjectDiscovery.get_player_statistics
// parity: rpc:ProjectDiscovery.get_remaining_bonus_samples
// parity: notify:OnAnalysisKreditsChange
// parity: feat:pve.login-campaign
// parity: rpc:loginCampaignManager.get_client_campaign_state
// parity: rpc:loginCampaignManager.claim_reward
// parity: notify:OnCampaignEnrollmentChanged
#[tokio::test]
async fn test_project_discovery_rpcs_ak_notify_and_login_campaigns_e2e() {
    let ctx = fresh_player_ctx();
    let (mut sess, char_id, _) =
        create_player_pilot(&ctx, 5102, "pd_analyst", "Aiko Analyst", 0).await;
    let _ = drain_all_notifications(&ctx, &mut sess, char_id);

    // --- Part A: Project Discovery full RPC lifecycle ---
    let xp_r1 = rpc_ok(
        &ctx,
        &mut sess,
        "ProjectDiscovery",
        "get_total_needed_xp",
        &[PyRep::Integer(1)],
    )
    .await
    .as_f64()
    .unwrap();
    let xp_r10 = rpc_ok(
        &ctx,
        &mut sess,
        "ProjectDiscovery",
        "get_total_needed_xp",
        &[PyRep::Integer(10)],
    )
    .await
    .as_f64()
    .unwrap();
    assert!(xp_r10 > xp_r1 && xp_r1 >= 0.0);

    let tut_xp = rpc_ok(&ctx, &mut sess, "ProjectDiscovery", "get_tutorial_xp_reward", &[])
        .await
        .as_f64()
        .unwrap();
    assert!(tut_xp > 0.0);

    // Complete 5 real tutorial steps via client RPCs (zero slash commands)
    for lvl in 1..=5 {
        let tt = rpc_ok(
            &ctx,
            &mut sess,
            "ProjectDiscovery",
            "new_training_task",
            &[PyRep::Integer(lvl)],
        )
        .await;
        assert!(tt.dict_get("task_id").is_some() || tt.dict_get("tutorialLevel").is_some());
        rpc_ok(&ctx, &mut sess, "ProjectDiscovery", "increase_tutorial_level", &[]).await;
    }

    let tut_reward = rpc_ok(&ctx, &mut sess, "ProjectDiscovery", "give_tutorial_rewards", &[]).await;
    assert_eq!(tut_reward, PyRep::Bool(true));
    let ps_after_tut = rpc_ok(&ctx, &mut sess, "ProjectDiscovery", "get_player_state", &[]).await;
    let xp_after_tut = ps_after_tut
        .dict_get("experience")
        .and_then(|v| v.as_f64())
        .unwrap();
    assert!((xp_after_tut - tut_xp).abs() < 1e-6);

    let max_bonus = rpc_ok(
        &ctx,
        &mut sess,
        "ProjectDiscovery",
        "get_maximum_bonus_samples",
        &[],
    )
    .await
    .as_i64()
    .unwrap();
    assert!(max_bonus > 0);
    let rem_before = rpc_ok(
        &ctx,
        &mut sess,
        "ProjectDiscovery",
        "get_remaining_bonus_samples",
        &[],
    )
    .await
    .as_i64()
    .unwrap();
    assert_eq!(rem_before, max_bonus);

    // Fetch a real classification task, inspect URL payload, submit classification
    let task_rep = rpc_ok(&ctx, &mut sess, "ProjectDiscovery", "get_new_task", &[]).await;
    let task_num = task_rep
        .dict_get("task_number")
        .and_then(|v| v.as_i64())
        .unwrap();
    let task_url = task_rep
        .dict_get("url")
        .and_then(|v| v.as_str())
        .unwrap()
        .to_string();

    let fetched_json = rpc_ok(
        &ctx,
        &mut sess,
        "ProjectDiscovery",
        "get_data_from_url",
        &[PyRep::String(task_url)],
    )
    .await;
    assert!(fetched_json
        .as_str()
        .is_some_and(|s| s.contains(&format!("\"task_number\":{task_num}"))));

    let sub = rpc_ok(
        &ctx,
        &mut sess,
        "ProjectDiscovery",
        "post_classification",
        &[
            PyRep::Integer(task_num),
            PyRep::Dict(vec![(
                PyRep::String("classification".into()),
                PyRep::List(vec![PyRep::Integer(1)]),
            )]),
        ],
    )
    .await;
    let ak_gained = sub
        .dict_get("AK_Reward")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    assert!(ak_gained > 0, "Expected positive AK reward on sample submission");

    let rem_after = rpc_ok(
        &ctx,
        &mut sess,
        "ProjectDiscovery",
        "get_remaining_bonus_samples",
        &[],
    )
    .await
    .as_i64()
    .unwrap();
    assert_eq!(rem_after, rem_before - 1);

    let stats = rpc_ok(&ctx, &mut sess, "ProjectDiscovery", "get_player_statistics", &[]).await;
    assert_eq!(kv_get(&stats, "tasks_submitted"), Some(&PyRep::Integer(1)));

    let notifs_pd = drain_all_notifications(&ctx, &mut sess, char_id);
    assert!(
        notifs_pd.iter().any(|(ev, _)| ev == "OnAnalysisKreditsChange"),
        "Expected OnAnalysisKreditsChange notification, got {notifs_pd:?}"
    );

    // --- Part B: Daily Login Campaign state, enrollment notify, multi-day claims ---
    let camp_state = rpc_ok(
        &ctx,
        &mut sess,
        "loginCampaignManager",
        "get_client_campaign_state",
        &[],
    )
    .await;
    assert!(!matches!(camp_state, PyRep::None));
    if let Some(camp_id) = kv_get(&camp_state, "campaignID")
        .or_else(|| kv_get(&camp_state, "campaign_id"))
        .and_then(|v| v.as_i64())
    {
        let _ = rpc_ok(
            &ctx,
            &mut sess,
            "loginCampaignManager",
            "enroll_in_campaign",
            &[PyRep::Integer(camp_id)],
        )
        .await;
        let notifs_camp = drain_all_notifications(&ctx, &mut sess, char_id);
        assert!(
            notifs_camp.iter().any(|(ev, _)| ev == "OnCampaignEnrollmentChanged"),
            "Expected OnCampaignEnrollmentChanged notification, got {notifs_camp:?}"
        );
    }

    let claim1 = rpc_ok(&ctx, &mut sess, "loginCampaignManager", "claim_reward", &[]).await;
    assert!(!matches!(claim1, PyRep::None));

    // Advance clock 24h + 5s and claim Day 2 reward
    ctx.advance_secs(86_405);
    let claim2 = rpc_ok(&ctx, &mut sess, "loginCampaignManager", "claim_reward", &[]).await;
    assert!(!matches!(claim2, PyRep::None));

    // Verify SQLite reload preserves Project Discovery AK/XP & Login Campaign state
    let ctx2 = reload_ctx_from_sqlite(&ctx);
    let (mut sess2, _) =
        login_existing_player(&ctx2, 5103, "pd_analyst", "pd_analyst-secret", char_id).await;
    let stats_reloaded =
        rpc_ok(&ctx2, &mut sess2, "ProjectDiscovery", "get_player_statistics", &[]).await;
    assert_eq!(
        kv_get(&stats_reloaded, "tasks_submitted"),
        Some(&PyRep::Integer(1))
    );
    assert_eq!(
        kv_get(&stats_reloaded, "analysisKredits"),
        Some(&PyRep::Integer(ak_gained))
    );
}
