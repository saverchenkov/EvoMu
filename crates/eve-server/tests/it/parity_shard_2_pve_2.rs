//! Genuine non-GM end-to-end integration tests for PvE Shard 2 (Part 2):
//! - Seasons, Milestones & Seasonal Login Campaigns (14 tags)
//! - Hacking Node Types, Failure Outcomes & Multi-Step Expeditions (6 tags)
//! - Capital CONCORD Rogue Analysis Beacon (CRAB) Link Activation & Interference (3 tags)

use std::collections::HashMap;
use eve_proto::PyRep;
use eve_server::rng::MechanicRng;
use eve_server::services::pve_crab_beacons::{
    get_system_interference_level_decayed, tick_crab_beacons_in_system, CrabBeaconPhase,
    CRAB_CEQ_COST, CRAB_INTERFERENCE_DELTA, CRAB_LINK_DURATION_SECS, TYPE_CARRIER_CRAB_BEACON,
    TYPE_CRAB_BEACON,
};
use eve_server::services::pve_incursion_lifecycle::sync_incursion_system_effect_beacons;
use eve_server::services::pve_incursion_sites::seed_incursion_for_constellation;
use eve_server::services::pve_special_sites::{read_pve_special_reg, with_pve_special_reg};
use eve_server::services::pve_exploration::{
    ensure_system_pve_sites_seeded, with_pve_exploration_reg, HackingNodeKind,
};
use crate::parity_shard_2_common::{
    bind_service, buy_fit_combat_ship, buy_item_at_station, configure_sqlite_blob_and_reload,
    create_player_pilot, drain_all_notifications, ensure_skills_for_types, fly_stargates_to,
    fresh_player_ctx, fund_pilot_isk, kv_get, login_existing_player, move_ship_to_ball,
    reload_ctx_from_sqlite, rpc_err, rpc_ok, rpc_res, undock_from_station, wait_for_warp_end,
    warp_and_engage_ball, JITA_STATION, JITA_SYS, LOWSEC_NOURVUKAIKEN_SYS,
};

fn dict_get<'a>(rep: &'a PyRep, key: &str) -> Option<&'a PyRep> {
    kv_get(rep, key)
}

// parity: rpc:seasonManager.get_max_active_challenges
// parity: rpc:seasonManager.reload_fsd_data
// parity: rpc:seasonManager.fill_up_and_return_challenges_for_character
// parity: rpc:seasonManager.get_challenge
// parity: rpc:seasonManager.challenge_is_dormant
// parity: rpc:seasonManager.get_challenge_expiration_date
// parity: rpc:seasonManager.process_client_event
// parity: rpc:seasonManager.claim_challenge_rewards
// parity: rpc:seasonManager.get_season_end_time
// parity: rpc:milestoneMgr.ProcessCharacterLogon
// parity: rpc:milestoneMgr.ClaimRewards
// parity: rpc:seasonalLoginCampaignManager.get_active_campaign
// parity: rpc:seasonalLoginCampaignManager.get_claim_history
// parity: rpc:seasonalLoginCampaignManager.claim_reward
#[tokio::test]
async fn test_seasons_milestones_and_seasonal_campaigns_e2e() {
    let ctx = fresh_player_ctx();
    let (mut sess, char_id, _ship_id) =
        create_player_pilot(&ctx, 51_001, "season_pilot_u1", "Season Runner", 0).await;

    let max_active = rpc_ok(&ctx, &mut sess, "seasonManager", "get_max_active_challenges", &[])
        .await
        .as_i64()
        .expect("max active challenges int");
    assert!(max_active >= 1, "SDE season must allow at least 1 active challenge");

    let season_end = rpc_ok(&ctx, &mut sess, "seasonManager", "get_season_end_time", &[])
        .await
        .as_i64()
        .unwrap_or(0);
    assert!(season_end > 0, "season end time must be positive filetime");

    // Non-GM pilot cannot reload FSD data
    let err_non_gm = rpc_err(&ctx, &mut sess, "seasonManager", "reload_fsd_data", &[]).await;
    assert!(!err_non_gm.code.is_empty());

    let filled = rpc_ok(&ctx, &mut sess, "seasonManager", "fill_up_and_return_challenges_for_character", &[]).await;
    let ch_list: Vec<PyRep> = match &filled {
        PyRep::List(items) | PyRep::Tuple(items) => items.clone(),
        PyRep::Dict(pairs) => pairs.iter().map(|(_, v)| v.clone()).collect(),
        other => panic!("expected challenge list, got {other:?}"),
    };
    assert!(!ch_list.is_empty(), "pilot must receive active season challenges");

    let all_fsd_defs = eve_server::services::season_fsd::fsd_challenge_definitions(&ctx.sde);
    let ch_id = ch_list
        .iter()
        .filter_map(|c| kv_get(c, "challengeID").and_then(|v| v.as_i64()))
        .find(|&id| {
            all_fsd_defs
                .iter()
                .find(|d| d.challenge_id == id)
                .is_none_or(|d| d.client_event_driven)
        })
        .or_else(|| kv_get(&ch_list[0], "challengeID").and_then(|v| v.as_i64()))
        .expect("challengeID");
    let def_opt = all_fsd_defs.iter().find(|d| d.challenge_id == ch_id);
    let single_ch = rpc_ok(&ctx, &mut sess, "seasonManager", "get_challenge", &[PyRep::Integer(ch_id)]).await;
    assert_eq!(kv_get(&single_ch, "challengeID").and_then(|v| v.as_i64()), Some(ch_id));
    let max_prog = kv_get(&single_ch, "maxProgress")
        .and_then(|v| v.as_i64())
        .or_else(|| def_opt.map(|d| d.max_progress))
        .unwrap_or(1)
        .max(1);
    let extra_data = def_opt
        .map(|d| {
            let pairs: Vec<(PyRep, PyRep)> = d
                .event_conditions
                .iter()
                .filter_map(|(k, vals)| vals.first().map(|&v| (PyRep::String(k.clone()), PyRep::Integer(v))))
                .collect();
            PyRep::Dict(pairs)
        })
        .unwrap_or_else(|| PyRep::Dict(Vec::new()));

    let dormant_before = rpc_ok(&ctx, &mut sess, "seasonManager", "challenge_is_dormant", &[PyRep::Integer(ch_id)]).await;
    assert_eq!(dormant_before, PyRep::Bool(false));

    let exp_date = rpc_ok(&ctx, &mut sess, "seasonManager", "get_challenge_expiration_date", &[PyRep::Integer(ch_id)])
        .await
        .as_i64()
        .unwrap_or(0);
    assert!(exp_date > 0);

    for _ in 0..max_prog {
        let _ = rpc_ok(
            &ctx,
            &mut sess,
            "seasonManager",
            "process_client_event",
            &[PyRep::Integer(ch_id), PyRep::Integer(1), extra_data.clone()],
        )
        .await;
    }
    let claim_rep = rpc_ok(&ctx, &mut sess, "seasonManager", "claim_challenge_rewards", &[PyRep::Integer(ch_id)]).await;
    assert_ne!(claim_rep, PyRep::None);

    // Milestones: ProcessCharacterLogon starts 120-min timer; early ClaimRewards rejected
    let _ = rpc_ok(&ctx, &mut sess, "milestoneMgr", "ProcessCharacterLogon", &[]).await;
    let m_notes = drain_all_notifications(&ctx, &mut sess, char_id);
    assert!(
        m_notes.iter().any(|(e, _)| e == "OnMilestoneTimerStarted"),
        "ProcessCharacterLogon must emit OnMilestoneTimerStarted"
    );
    let early_err = rpc_err(&ctx, &mut sess, "milestoneMgr", "ClaimRewards", &[PyRep::Integer(1)]).await;
    assert_eq!(early_err.code, "MilestonePrerequisiteNotMet");

    ctx.advance_secs(120 * 60 + 5);
    let bal_before_ms = ctx.db.get_character(char_id).map(|c| c.balance.raw_cents()).unwrap_or(0);
    rpc_ok(&ctx, &mut sess, "milestoneMgr", "ClaimRewards", &[PyRep::Integer(1)]).await;
    let bal_after_ms = ctx.db.get_character(char_id).map(|c| c.balance.raw_cents()).unwrap_or(0);
    assert!(bal_after_ms > bal_before_ms, "Milestone ClaimRewards must credit ISK reward");

    // Seasonal Login Campaign configured via real SQLite service_state_blobs + roundtrip
    let sde_campaigns = eve_server::services::campaigns_and_milestones::get_sde_login_campaigns(&ctx.sde);
    let (&camp_id, _) = sde_campaigns.iter().next().expect("SDE seasonal login campaign");
    let now = ctx.now_unix();
    let sched_json = format!(
        r#"{{"campaign_id":{camp_id},"start_unix":{},"end_unix":{},"turnover_secs":39600}}"#,
        now.saturating_sub(3600),
        now + 86_400 * 7
    );
    let ctx2 = configure_sqlite_blob_and_reload(&ctx, "seasonal_login_schedule", &sched_json);
    let (mut sess2, _) = login_existing_player(&ctx2, 51_003, "season_pilot_u1", "season_pilot_u1-secret", char_id).await;

    let active_camp = rpc_ok(&ctx2, &mut sess2, "seasonalLoginCampaignManager", "get_active_campaign", &[]).await;
    match &active_camp {
        PyRep::Tuple(tup) => assert_eq!(tup.first().and_then(|v| v.as_i64()), Some(camp_id)),
        other => panic!("expected seasonal campaign tuple, got {other:?}"),
    }
    let claimed = rpc_ok(&ctx2, &mut sess2, "seasonalLoginCampaignManager", "claim_reward", &[]).await;
    assert_ne!(claimed, PyRep::None);

    let ctx3 = reload_ctx_from_sqlite(&ctx2);
    let (mut sess3, _) = login_existing_player(&ctx3, 51_004, "season_pilot_u1", "season_pilot_u1-secret", char_id).await;
    let hist = rpc_ok(&ctx3, &mut sess3, "seasonalLoginCampaignManager", "get_claim_history", &[]).await;
    match &hist {
        PyRep::Dict(entries) => assert_eq!(entries.len(), 1, "SQLite reload must preserve 1 seasonal claim day"),
        other => panic!("expected claim history dict, got {other:?}"),
    }
}

// parity: feat:pve.hacking-node-types
// parity: feat:pve.hacking-failure-outcome
// parity: feat:pve.escalations
// parity: feat:pve.expeditions
// parity: notify:OnDungeonInstanceAdded
// parity: notify:OnDungeonInstanceRemoved
#[tokio::test]
async fn test_hacking_nodes_failure_outcomes_and_expeditions_e2e() {
    let ctx = fresh_player_ctx();
    let low_seed = (1_u64..1000).find(|&s| MechanicRng::seeded(s).try_unit_f64().unwrap() < 0.05).unwrap_or(42);
    ctx.mechanic_rng.set_seed(Some(low_seed));

    let (mut sess, char_id, _pod) = create_player_pilot(&ctx, 51_101, "hacker_exp_u1", "Explorer Ace", JITA_SYS).await;
    let analyzer_tid = ctx
        .sde
        .types
        .values()
        .filter(|t| t.published && t.category_id == 7 && t.name == "Data Analyzer I")
        .map(|t| t.type_id)
        .next()
        .unwrap_or(22_177);
    let (ship_id, guns) = buy_fit_combat_ship(&ctx, &mut sess, char_id, JITA_STATION, 644, 494, 205, 6).await;
    let data_analyzer_id = buy_item_at_station(&ctx, &mut sess, char_id, JITA_STATION, analyzer_tid, 1).await;
    rpc_ok(&ctx, &mut sess, "invbroker", "Add", &[PyRep::Integer(data_analyzer_id), PyRep::Integer(ship_id), PyRep::Integer(19)]).await;

    undock_from_station(&ctx, &mut sess, JITA_SYS).await;
    fly_stargates_to(&ctx, &mut sess, LOWSEC_NOURVUKAIKEN_SYS).await;
    ensure_system_pve_sites_seeded(&ctx, LOWSEC_NOURVUKAIKEN_SYS).await;

    let (data_site_id, data_can_id, ghost_site_id, ghost_can_id, anomaly_id) =
        with_pve_exploration_reg(&ctx, |reg| {
            let sites = &reg.sites_by_system[&LOWSEC_NOURVUKAIKEN_SYS];
            let ds = sites
                .iter()
                .find(|s| (s.site_category == "Data" || s.site_category == "Relic") && !s.hackable_can_ids.is_empty())
                .expect("seeded data/relic container");
            let gs = sites
                .iter()
                .find(|s| s.site_category == "Ghost" && !s.hackable_can_ids.is_empty())
                .map(|s| (s.site_id.clone(), s.hackable_can_ids[0]));
            let anom = sites
                .iter()
                .find(|s| (s.site_category == "CombatAnomaly" || s.site_category == "CombatSignature") && !s.active_npc_ball_ids.is_empty())
                .expect("seeded combat anomaly");
            (ds.site_id.clone(), ds.hackable_can_ids[0], gs.as_ref().map(|g| g.0.clone()), gs.map(|g| g.1), anom.site_id.clone())
        });

    move_ship_to_ball(&ctx, &mut sess, LOWSEC_NOURVUKAIKEN_SYS, ship_id, "dungeon", PyRep::String(data_site_id)).await;
    move_ship_to_ball(&ctx, &mut sess, LOWSEC_NOURVUKAIKEN_SYS, ship_id, "item", PyRep::Integer(data_can_id)).await;

    let start_rep = rpc_ok(&ctx, &mut sess, "hackingMgr", "StartNewGameInstance", &[PyRep::Integer(data_can_id)]).await;
    assert_eq!(dict_get(&start_rep, "status").and_then(|v| v.as_str()), Some("InProgress"));
    let _ = rpc_ok(&ctx, &mut sess, "hackingMgr", "GetMyHackProgress", &[]).await;

    let (has_core, has_def, has_util) = with_pve_exploration_reg(&ctx, |reg| {
        let g = &reg.hacking_games[&(char_id, data_can_id)];
        let core = g.tiles.iter().any(|t| t.kind == HackingNodeKind::SystemCore);
        let def = g.tiles.iter().any(|t| matches!(t.kind, HackingNodeKind::Firewall | HackingNodeKind::Antivirus | HackingNodeKind::RestorationNode | HackingNodeKind::VirusSuppressor));
        let util = g.tiles.iter().any(|t| t.kind == HackingNodeKind::UtilitySubsystem);
        (core, def, util)
    });
    assert!(has_core && has_def && has_util, "hacking grid must spawn SystemCore, Defensive, and UtilitySubsystem nodes");

    let _ = rpc_ok(&ctx, &mut sess, "hackingMgr", "ClickedOnTile", &[PyRep::Integer(data_can_id), PyRep::Integer(1)]).await;
    let _ = rpc_res(&ctx, &mut sess, "hackingMgr", "UsedUtilityElement", &[PyRep::Integer(0)]).await;
    let _ = rpc_res(&ctx, &mut sess, "hackingMgr", "UsedUtilityElementOnVirus", &[PyRep::Integer(0)]).await;

    // 1st quit leaves container intact; 2nd failed attempt detonates standard container
    let _ = rpc_ok(&ctx, &mut sess, "hackingMgr", "QuitHackingAttempt", &[PyRep::Integer(data_can_id)]).await;
    let (fc1, hacked1) = with_pve_exploration_reg(&ctx, |reg| {
        (
            reg.failure_counts.get(&(LOWSEC_NOURVUKAIKEN_SYS, data_can_id)).copied().unwrap_or(0),
            reg.hacked_containers.contains(&(LOWSEC_NOURVUKAIKEN_SYS, data_can_id)),
        )
    });
    assert_eq!(fc1, 1, "1st fail records failure_counts=1");
    assert!(!hacked1, "1st fail keeps container intact");
    let _ = rpc_ok(&ctx, &mut sess, "hackingMgr", "StartNewGameInstance", &[PyRep::Integer(data_can_id)]).await;
    let _ = rpc_ok(&ctx, &mut sess, "hackingMgr", "QuitHackingAttempt", &[PyRep::Integer(data_can_id)]).await;
    let (fc2, hacked2) = with_pve_exploration_reg(&ctx, |reg| {
        (
            reg.failure_counts.get(&(LOWSEC_NOURVUKAIKEN_SYS, data_can_id)).copied().unwrap_or(0),
            reg.hacked_containers.contains(&(LOWSEC_NOURVUKAIKEN_SYS, data_can_id)),
        )
    });
    assert_eq!(fc2, 2, "2nd fail records failure_counts=2");
    assert!(hacked2, "2nd fail detonates container");

    if let (Some(gsid), Some(gcid)) = (ghost_site_id, ghost_can_id) {
        move_ship_to_ball(&ctx, &mut sess, LOWSEC_NOURVUKAIKEN_SYS, ship_id, "dungeon", PyRep::String(gsid)).await;
        move_ship_to_ball(&ctx, &mut sess, LOWSEC_NOURVUKAIKEN_SYS, ship_id, "item", PyRep::Integer(gcid)).await;
        let _ = rpc_res(&ctx, &mut sess, "hackingMgr", "StartNewGameInstance", &[PyRep::Integer(gcid)]).await;
        ctx.advance_secs(180);
        let _ = rpc_res(&ctx, &mut sess, "hackingMgr", "QuitHackingAttempt", &[PyRep::Integer(gcid)]).await;
    }

    // Clear combat anomaly -> trigger Escalation -> OnDungeonInstanceAdded + GetMyEscalatingPathDetails + expedition chain
    let esc_dest_sys = eve_server::services::pve_expeditions::find_escalation_destination_system(&ctx, LOWSEC_NOURVUKAIKEN_SYS);
    let (mut obs_sess, obs_cid, _) = create_player_pilot(&ctx, 51_102, "hacker_obs_u2", "Observer Pilot", 0).await;
    fly_stargates_to(&ctx, &mut obs_sess, esc_dest_sys).await;
    let _ = drain_all_notifications(&ctx, &mut obs_sess, obs_cid);
    let _ = drain_all_notifications(&ctx, &mut sess, char_id);
    for _wave in 0..8 {
        let wave_npcs = with_pve_exploration_reg(&ctx, |reg| {
            reg.sites_by_system.get(&LOWSEC_NOURVUKAIKEN_SYS)
                .and_then(|v| v.iter().find(|s| s.site_id == anomaly_id && !s.cleared))
                .map(|s| s.active_npc_ball_ids.clone())
                .unwrap_or_default()
        });
        if wave_npcs.is_empty() { break; }
        for (idx, &npc_id) in wave_npcs.iter().enumerate() {
            if idx + 1 == wave_npcs.len() { ctx.mechanic_rng.set_seed(Some(low_seed)); }
            warp_and_engage_ball(&ctx, &mut sess, LOWSEC_NOURVUKAIKEN_SYS, ship_id, &guns, npc_id, "dungeon", PyRep::String(anomaly_id.clone())).await;
        }
    }

    let mut notes_added = drain_all_notifications(&ctx, &mut obs_sess, obs_cid);
    notes_added.extend(drain_all_notifications(&ctx, &mut sess, char_id));
    assert!(notes_added.iter().any(|(ev, _)| ev == "OnDungeonInstanceAdded"), "escalation must emit OnDungeonInstanceAdded");

    let esc_details = rpc_ok(&ctx, &mut sess, "dungeonExplorationMgr", "GetMyEscalatingPathDetails", &[]).await;
    let esc_inst_id = match &esc_details {
        PyRep::Rowset { columns, rows } => {
            assert!(!rows.is_empty(), "pilot must have active escalation");
            let ci = columns.iter().position(|c| c == "instanceID").unwrap_or(0);
            rows[0][ci].as_i64().expect("instanceID")
        }
        PyRep::List(r) => {
            assert!(!r.is_empty(), "pilot must have active escalation");
            dict_get(&r[0], "instanceID").or_else(|| dict_get(&r[0], "journalEntryID")).and_then(|v| v.as_i64()).expect("instanceID")
        }
        _ => panic!("expected escalation list or rowset, got {esc_details:?}"),
    };

    let mut notes_removed = Vec::new();
    for _step in 0..5 {
        let cur_path = with_pve_exploration_reg(&ctx, |reg| {
            reg.escalations_by_char.get(&char_id).and_then(|v| v.iter().find(|e| e.instance_id == esc_inst_id)).cloned()
        });
        let Some(entry) = cur_path else { break };
        let dest_sys = entry.destination_solar_system_id;
        fly_stargates_to(&ctx, &mut sess, dest_sys).await;
        let b_oid = bind_service(&ctx, &mut sess, "beyonce", PyRep::Integer(i64::from(dest_sys))).await as u64;
        ctx.dispatch_rpc(&mut sess, "beyonce", Some(b_oid), "CmdWarpToStuff", &[PyRep::String("epinstance".into()), PyRep::Integer(esc_inst_id)], &HashMap::new(), None).await.unwrap();
        wait_for_warp_end(&ctx, dest_sys, ship_id).await;
        let expected_site_id = format!("EXP-{}-{}", entry.instance_id, entry.step_index);
        for _wave in 0..8 {
            let step_npcs = with_pve_exploration_reg(&ctx, |reg| {
                reg.sites_by_system.get(&dest_sys)
                    .and_then(|v| v.iter().find(|s| s.site_id == expected_site_id && !s.cleared))
                    .map(|s| s.active_npc_ball_ids.clone())
                    .unwrap_or_default()
            });
            if step_npcs.is_empty() { break; }
            for nid in step_npcs {
                warp_and_engage_ball(&ctx, &mut sess, dest_sys, ship_id, &guns, nid, "dungeon", PyRep::String(expected_site_id.clone())).await;
            }
        }
        notes_removed.extend(drain_all_notifications(&ctx, &mut sess, char_id));
        notes_removed.extend(drain_all_notifications(&ctx, &mut obs_sess, obs_cid));
    }

    notes_removed.extend(drain_all_notifications(&ctx, &mut sess, char_id));
    notes_removed.extend(drain_all_notifications(&ctx, &mut obs_sess, obs_cid));
    assert!(notes_removed.iter().any(|(ev, _)| ev == "OnDungeonInstanceRemoved"), "expedition completion must emit OnDungeonInstanceRemoved");
}

// parity: feat:pve.crab-link-activation
// parity: feat:pve.crab-ceq-signal-interference
// parity: notify:OnSolarsystemInterferenceChanged
#[tokio::test]
async fn test_crab_beacon_activation_ceq_interference_notify_e2e() {
    let ctx = fresh_player_ctx();
    let (mut sess, char_id, _pod) = create_player_pilot(&ctx, 51_201, "crab_carrier_u1", "Carrier Pilot", JITA_SYS).await;
    let (mut escort_sess, escort_cid, _) = create_player_pilot(&ctx, 51_203, "crab_escort_u3", "Dread Escort", JITA_SYS).await;
    let (mut cyno_sess, cyno_cid, _) = create_player_pilot(&ctx, 51_202, "crab_cyno_u2", "Cyno Pilot", JITA_SYS).await;
    fund_pilot_isk(&ctx, &mut sess, 6).await;
    fund_pilot_isk(&ctx, &mut escort_sess, 4).await;

    ensure_skills_for_types(&ctx, &mut sess, char_id, JITA_STATION, &[28_352, 17_887, 3_456, 1319, TYPE_CRAB_BEACON, 28_756]).await;
    let carrier_id = buy_item_at_station(&ctx, &mut sess, char_id, JITA_STATION, 28_352, 1).await;
    ctx.advance_secs(11);
    rpc_ok(&ctx, &mut sess, "ship", "AssembleShip", &[PyRep::List(vec![PyRep::Integer(carrier_id)])]).await;
    rpc_ok(&ctx, &mut sess, "ship", "Board", &[PyRep::Integer(carrier_id)]).await;
    ctx.advance_secs(11);
    sess.ship_id = Some(carrier_id);
    for slot in 11..=13 {
        let expander_id = buy_item_at_station(&ctx, &mut sess, char_id, JITA_STATION, 1319, 1).await;
        rpc_ok(&ctx, &mut sess, "invbroker", "Add", &[PyRep::Integer(expander_id), PyRep::Integer(carrier_id), PyRep::Integer(slot)]).await;
    }
    let fuel_id = buy_item_at_station(&ctx, &mut sess, char_id, JITA_STATION, 17_887, 25_000).await;
    rpc_ok(&ctx, &mut sess, "invbroker", "Add", &[PyRep::Integer(fuel_id), PyRep::Integer(carrier_id), PyRep::Integer(133)]).await;

    let beacon_stack = buy_item_at_station(&ctx, &mut sess, char_id, JITA_STATION, TYPE_CRAB_BEACON, 4).await;
    rpc_ok(&ctx, &mut sess, "invbroker", "Add", &[PyRep::Integer(beacon_stack), PyRep::Integer(carrier_id), PyRep::Integer(5)]).await;

    let (escort_ship, escort_guns) = buy_fit_combat_ship(&ctx, &mut escort_sess, escort_cid, JITA_STATION, 642, 2929, 12_779, 6).await;
    let esc_fuel = buy_item_at_station(&ctx, &mut escort_sess, escort_cid, JITA_STATION, 16_274, 25_000).await;
    rpc_ok(&ctx, &mut escort_sess, "invbroker", "Add", &[PyRep::Integer(esc_fuel), PyRep::Integer(escort_ship), PyRep::Integer(5)]).await;

    ensure_skills_for_types(&ctx, &mut cyno_sess, cyno_cid, JITA_STATION, &[22_428, 21_096]).await;
    let cyno_ship = buy_item_at_station(&ctx, &mut cyno_sess, cyno_cid, JITA_STATION, 22_428, 1).await;
    ctx.advance_secs(11);
    rpc_ok(&ctx, &mut cyno_sess, "ship", "AssembleShip", &[PyRep::List(vec![PyRep::Integer(cyno_ship)])]).await;
    rpc_ok(&ctx, &mut cyno_sess, "ship", "Board", &[PyRep::Integer(cyno_ship)]).await;
    ctx.advance_secs(11);
    cyno_sess.ship_id = Some(cyno_ship);
    let cyno_mod = buy_item_at_station(&ctx, &mut cyno_sess, cyno_cid, JITA_STATION, 21_096, 1).await;
    rpc_ok(&ctx, &mut cyno_sess, "invbroker", "Add", &[PyRep::Integer(cyno_mod), PyRep::Integer(cyno_ship), PyRep::Integer(27)]).await;
    let ozone = buy_item_at_station(&ctx, &mut cyno_sess, cyno_cid, JITA_STATION, 16_273, 1_500).await;
    rpc_ok(&ctx, &mut cyno_sess, "invbroker", "Add", &[PyRep::Integer(ozone), PyRep::Integer(cyno_ship), PyRep::Integer(5)]).await;

    let _ = rpc_ok(&ctx, &mut sess, "fleetMgr", "CreateFleet", &[]).await;
    for (fcid, fsess) in [(cyno_cid, &mut cyno_sess), (escort_cid, &mut escort_sess)] {
        let _ = rpc_res(&ctx, &mut sess, "fleetMgr", "Invite", &[PyRep::Integer(fcid), PyRep::Integer(4)]).await;
        let _ = rpc_res(&ctx, fsess, "fleetMgr", "AcceptInvite", &[]).await;
    }

    let jita_pos = glam::DVec3::from_array(ctx.sde.solar_systems[&JITA_SYS].galactic_pos_ly);
    let crab_sys = ctx.sde.solar_systems.values()
        .filter(|s| s.solar_system_id < 31_000_000 && s.region_id != 10_000_070 && s.security_status < 0.45 && glam::DVec3::from_array(s.galactic_pos_ly).distance(jita_pos) <= 4.9)
        .min_by(|a, b| glam::DVec3::from_array(a.galactic_pos_ly).distance(jita_pos).partial_cmp(&glam::DVec3::from_array(b.galactic_pos_ly).distance(jita_pos)).unwrap())
        .map(|s| s.solar_system_id)
        .unwrap_or(30_002_813);

    undock_from_station(&ctx, &mut cyno_sess, JITA_SYS).await;
    fly_stargates_to(&ctx, &mut cyno_sess, crab_sys).await;
    let safe_cel = ctx
        .sde
        .celestials_in_system(crab_sys)
        .into_iter()
        .find(|c| {
            matches!(c.group_id, 6 | 7)
                && ctx.sde.jumps.values().filter(|j| j.from_solar_system_id == crab_sys).all(|j| j.position.distance(c.position) > 2_000_000.0)
                && ctx.sde.stations.values().filter(|s| s.solar_system_id == crab_sys).all(|s| s.position.distance(c.position) > 2_000_000.0)
        })
        .expect("safe celestial in crab_sys");
    let cyno_b = bind_service(&ctx, &mut cyno_sess, "beyonce", PyRep::Integer(i64::from(crab_sys))).await as u64;
    let _ = ctx.dispatch_rpc(
        &mut cyno_sess, "beyonce", Some(cyno_b), "CmdWarpToStuff",
        &[PyRep::String("item".into()), PyRep::Integer(safe_cel.celestial_id as i64)],
        &HashMap::new(), None,
    ).await;
    wait_for_warp_end(&ctx, crab_sys, cyno_ship).await;
    ctx.advance_secs(35);
    let cyno_d = bind_service(&ctx, &mut cyno_sess, "dogmaIM", PyRep::Integer(cyno_ship)).await as u64;
    let cyno_rep = ctx.dispatch_rpc(
        &mut cyno_sess, "dogmaIM", Some(cyno_d), "Activate",
        &[PyRep::Integer(cyno_mod), PyRep::String("cynosuralGeneration".into())],
        &HashMap::new(), None,
    ).await.unwrap();
    let cyno_beacon_id = dict_get(&cyno_rep, "beaconID").and_then(|v| v.as_i64()).expect("cyno beaconID");

    undock_from_station(&ctx, &mut sess, JITA_SYS).await;
    ctx.advance_secs(35);
    let jita_b = bind_service(&ctx, &mut sess, "beyonce", PyRep::Integer(i64::from(JITA_SYS))).await as u64;
    ctx.dispatch_rpc(
        &mut sess, "beyonce", Some(jita_b), "CmdJumpToFleetModuleBeacon",
        &[PyRep::Integer(cyno_cid), PyRep::Integer(i64::from(crab_sys)), PyRep::Integer(cyno_beacon_id)],
        &HashMap::new(), None,
    ).await.unwrap();
    sess.solar_system_id = Some(crab_sys);
    let _ = bind_service(&ctx, &mut sess, "beyonce", PyRep::Integer(i64::from(crab_sys))).await;
    fly_stargates_to(&ctx, &mut escort_sess, crab_sys).await;
    let nour_b_oid = bind_service(&ctx, &mut sess, "beyonce", PyRep::Integer(i64::from(crab_sys))).await as u64;
    let crab_sys_h = ctx.systems.get_or_spawn(crab_sys);
    for cap_sess in [&mut sess, &mut escort_sess] {
        let b = bind_service(&ctx, cap_sess, "beyonce", PyRep::Integer(i64::from(crab_sys))).await as u64;
        let _ = ctx.dispatch_rpc(cap_sess, "beyonce", Some(b), "CmdSetSpeedFraction", &[PyRep::Real(0.1)], &HashMap::new(), None).await;
    }
    let _ = crab_sys_h.step_manual(2.0).await;
    for cap_sess in [&mut sess, &mut escort_sess] {
        let b = bind_service(&ctx, cap_sess, "beyonce", PyRep::Integer(i64::from(crab_sys))).await as u64;
        let _ = ctx.dispatch_rpc(cap_sess, "beyonce", Some(b), "CmdStop", &[], &HashMap::new(), None).await;
    }
    let _ = crab_sys_h.step_manual(1.0).await;
    let _ = drain_all_notifications(&ctx, &mut sess, char_id);
    let ship_pos = crab_sys_h.get_ball_position_sync(carrier_id).expect("carrier in crab_sys");
    let anchor_offsets = [(-4_500.0, 0.0), (0.0, 4_900.0), (2_000.0, 0.0), (0.0, -4_900.0)];
    let mut prev_mod = 1.0_f64;

    for idx in 0..4 {
        let bid = if idx < 3 {
            let sp = rpc_ok(
                &ctx,
                &mut sess,
                "invbroker",
                "SplitStack",
                &[PyRep::Integer(beacon_stack), PyRep::Integer(1)],
            )
            .await;
            dict_get(&sp, "newItemID")
                .and_then(|v| v.as_i64())
                .expect("split CRAB beacon newItemID")
        } else {
            beacon_stack
        };
        let (dx, dy) = anchor_offsets[idx];
        let apos = PyRep::Tuple(vec![PyRep::Real(ship_pos.x + dx), PyRep::Real(ship_pos.y + dy), PyRep::Real(ship_pos.z)]);
        rpc_ok(&ctx, &mut sess, "posMgr", "AnchorStructure", &[PyRep::Integer(bid), apos]).await;
        ctx.advance_secs(2);
        let _ = crab_sys_h.step_manual(2.0).await;
        if idx < 3 {
            let link_rep = ctx.dispatch_rpc(
                &mut sess, "beyonce", Some(nour_b_oid), "CallComponentFromClient",
                &[PyRep::Integer(bid), PyRep::String("linkWithShip".into()), PyRep::String("InitiateLink".into())],
                &HashMap::new(), None,
            ).await.unwrap();
            assert_eq!(dict_get(&link_rep, "phase").and_then(|v| v.as_str()), Some("linking"));
            assert_eq!(dict_get(&link_rep, "linked"), Some(&PyRep::Bool(true)));
            let ceq_cost = dict_get(&link_rep, "ceqCost").and_then(|v| v.as_i64()).unwrap_or(0);
            let cost_mod = dict_get(&link_rep, "interferenceCostModifier").and_then(|v| v.as_f64()).unwrap_or(0.0);
            if idx == 0 {
                assert!(ceq_cost >= CRAB_CEQ_COST && ceq_cost <= CRAB_CEQ_COST + 5, "initial CEQ cost near base CRAB_CEQ_COST ({ceq_cost})");
                assert!((cost_mod - 1.0).abs() < 0.05, "initial cost modifier near 1.0 ({cost_mod})");
            } else {
                assert!(cost_mod > prev_mod && ceq_cost >= CRAB_CEQ_COST, "CEQ Cost Modifier must scale up with interference ({cost_mod} vs {prev_mod})");
            }
            prev_mod = cost_mod;
            ctx.advance_secs(CRAB_LINK_DURATION_SECS + 5);
            tick_crab_beacons_in_system(&ctx, crab_sys).await;
            if idx == 2 {
                for _w in 0..4 {
                    let drones = read_pve_special_reg(&ctx, |reg| reg.crab_beacons.get(&bid).map(|r| r.wave_npc_ball_ids.clone()).unwrap_or_default());
                    for did in drones {
                        warp_and_engage_ball(&ctx, &mut escort_sess, crab_sys, escort_ship, &escort_guns, did, "item", PyRep::Integer(bid)).await;
                    }
                    ctx.advance_secs(210);
                    tick_crab_beacons_in_system(&ctx, crab_sys).await;
                    if read_pve_special_reg(&ctx, |reg| reg.crab_beacons.get(&bid).map(|r| r.phase)) == Some(CrabBeaconPhase::LootRestricted) { break; }
                }
                let phase = read_pve_special_reg(&ctx, |reg| reg.crab_beacons.get(&bid).map(|r| r.phase));
                assert_eq!(phase, Some(CrabBeaconPhase::LootRestricted), "clearing Rogue Drone waves + 10m scan must enter LootRestricted");
                let loot_id = ctx.db.with_snapshot(|s| s.items.values().find(|i| i.location_id == bid).map(|i| i.item_id)).expect("CRAB SDE loot items");
                let non_owner_err = rpc_err(&ctx, &mut escort_sess, "invbroker", "Add", &[PyRep::Integer(loot_id), PyRep::Integer(escort_ship), PyRep::Integer(5)]).await;
                assert_eq!(non_owner_err.code, "LootOwnershipRestricted", "non-deployer must be blocked during 2m exclusive loot window");
                rpc_ok(&ctx, &mut sess, "invbroker", "Add", &[PyRep::Integer(loot_id), PyRep::Integer(carrier_id), PyRep::Integer(5)]).await;
                assert_eq!(ctx.db.get_item(loot_id).map(|i| i.location_id), Some(carrier_id), "deployer must loot CRAB reward via invbroker");
            }
        } else {
            let err_ceq = ctx.dispatch_rpc(
                &mut sess, "beyonce", Some(nour_b_oid), "CallComponentFromClient",
                &[PyRep::Integer(bid), PyRep::String("linkWithShip".into()), PyRep::String("InitiateLink".into())],
                &HashMap::new(), None,
            ).await.expect_err("4th CRAB beacon refused for insufficient CEQ");
            assert_eq!(err_ceq.code, "InsufficientEnergy");

            let crab_const = ctx.sde.solar_systems[&crab_sys].constellation_id;
            with_pve_special_reg(&ctx, |reg| seed_incursion_for_constellation(&ctx, reg, crab_const));
            sync_incursion_system_effect_beacons(&ctx);
            let err_jam = ctx.dispatch_rpc(
                &mut sess, "beyonce", Some(nour_b_oid), "CallComponentFromClient",
                &[PyRep::Integer(bid), PyRep::String("linkWithShip".into()), PyRep::String("InitiateLink".into())],
                &HashMap::new(), None,
            ).await.expect_err("system-wide Cynosural Jammer must block CRAB link activation");
            assert_eq!(err_jam.code, "CynoJammed");
        }
    }

    let notes = drain_all_notifications(&ctx, &mut sess, char_id);
    assert!(notes.iter().any(|(ev, _)| ev == "OnSolarsystemInterferenceChanged"), "must emit OnSolarsystemInterferenceChanged");

    let level = get_system_interference_level_decayed(&ctx, crab_sys, ctx.now_unix());
    assert!(level > 0.50 && level <= 0.35 + 3.0 * CRAB_INTERFERENCE_DELTA + 0.01, "interference must reflect cyno + 3 CRAB links, got {level}");

    let ctx2 = reload_ctx_from_sqlite(&ctx);
    let persisted_level = get_system_interference_level_decayed(&ctx2, crab_sys, ctx2.now_unix());
    assert!(persisted_level > 0.20 && TYPE_CARRIER_CRAB_BEACON > 0);
    ctx2.advance_secs(12 * 3_600);
    let decayed_level = get_system_interference_level_decayed(&ctx2, crab_sys, ctx2.now_unix());
    assert!(decayed_level < persisted_level, "system Signal Interference must decay over 25h window ({decayed_level} < {persisted_level})");
}
