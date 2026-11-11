//! Extra branch-coverage and mutation-killing unit tests for `services::campaigns_and_milestones`.

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::account_services::account_services_tests::{auth_session, test_ctx};
use crate::services::campaigns_and_milestones::*;
use crate::services::cosmetics_ui_ops::{
    SeasonalCampaignSchedule, load_cosmetics_ui_state, save_cosmetics_ui_state,
};
use eve_machonet::{SessionState, service_roles};
use eve_proto::{BridgeRpcError, PyRep};
use std::collections::{BTreeMap, HashMap};

async fn call_login_campaign(
    ctx: &ServerContext,
    session: &mut SessionState,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let kw = HashMap::new();
    handle(ctx, session, "loginCampaignManager", method, args, &kw)
        .await
        .expect("loginCampaignManager method must be dispatched")
}

async fn call_seasonal_campaign(
    ctx: &ServerContext,
    session: &mut SessionState,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let kw = HashMap::new();
    handle(ctx, session, "seasonalLoginCampaignManager", method, args, &kw)
        .await
        .expect("seasonalLoginCampaignManager method must be dispatched")
}

async fn call_info_gathering(
    ctx: &ServerContext,
    session: &mut SessionState,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let kw = HashMap::new();
    handle(ctx, session, "infoGatheringMgr", method, args, &kw)
        .await
        .expect("infoGatheringMgr method must be dispatched")
}

async fn call_milestone_mgr(
    ctx: &ServerContext,
    session: &mut SessionState,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let kw = HashMap::new();
    handle(ctx, session, "milestoneMgr", method, args, &kw)
        .await
        .expect("milestoneMgr method must be dispatched")
}

async fn call_community_track(
    ctx: &ServerContext,
    session: &mut SessionState,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let kw = HashMap::new();
    handle(ctx, session, "communityTrackMgr", method, args, &kw)
        .await
        .expect("communityTrackMgr method must be dispatched")
}

#[test]
fn pure_date_turnover_and_track_length_helpers_kill_boundary_mutants() {
    assert_eq!(format_turnover_hms(0), "00:00:00");
    assert_eq!(format_turnover_hms(86_399), "23:59:59");
    assert_eq!(format_turnover_hms(86_400), "00:00:00");
    assert_eq!(parse_turnover_hms("  00:00:00 "), Some(0));
    assert_eq!(parse_turnover_hms("23:59:59"), Some(86_399));
    assert_eq!(parse_turnover_hms("24:00:00"), None);
    assert_eq!(parse_turnover_hms("12:60:00"), None);
    assert_eq!(parse_turnover_hms("12:00:60"), None);
    assert_eq!(parse_turnover_hms("xx:00:00"), None);
    assert_eq!(parse_turnover_hms("12:xx:00"), None);
    assert_eq!(parse_turnover_hms("12:00:xx"), None);
    assert_eq!(parse_turnover_hms("12:00:00:00"), None);

    assert_eq!(eve_downtime_day_bucket(39_599), 0);
    assert_eq!(eve_downtime_day_bucket(39_600), 0);
    assert_eq!(eve_downtime_day_bucket(39_600 + 86_399), 0);
    assert_eq!(eve_downtime_day_bucket(39_600 + 86_400), 1);

    assert_eq!(
        seasonal_reward_date_ordinal(39_599, 39_600),
        GREGORIAN_EPOCH_ORDINAL - 1
    );
    assert_eq!(
        seasonal_reward_date_ordinal(39_600, 39_600),
        GREGORIAN_EPOCH_ORDINAL
    );

    assert_eq!(format_unix_utc(1_709_209_896), "2024-02-29 12:31:36");
    assert_eq!(format_unix_utc(1_721_001_600), "2024-07-15 00:00:00");

    let mut omega_map = BTreeMap::new();
    for d in 0..5 {
        omega_map.insert(
            d,
            SdeLoginRewardEntry {
                type_id: 34,
                quantity: 1,
                tier: 1,
                blueprint_runs: None,
                blueprint_material_level: None,
                blueprint_productivity_level: None,
                redeemed_expiry_days: None,
                is_auto_injected_booster_or_skin: false,
                overlay_texture_path: None,
            },
        );
    }
    let camp = SdeLoginCampaign {
        campaign_id: 9999,
        campaign_name_id: 1,
        auto_claim: false,
        retro_claim: true,
        window_header_background: None,
        alpha_rewards: BTreeMap::new(),
        omega_rewards: omega_map,
    };
    assert_eq!(camp.track_length(), 5);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn login_and_seasonal_campaign_omega_retro_claim_undocked_redeem_and_error_branches() {
    let ctx = test_ctx();
    let uid = 1_u64;
    let cid = 90_000_001_i64;
    let mut sess = auth_session(uid, cid, 98_000_001, 30_000_142);

    // 1. Custom per-character enrollment to unknown campaign -> CampaignNotFound
    ctx.db.with_mut_snapshot(|snap| {
        snap.seasons_and_campaigns
            .login_campaign_enrollments
            .insert(cid, 999_999_999_i64);
    });
    assert_eq!(
        call_login_campaign(&ctx, &mut sess, "get_client_campaign_state", &[])
            .await
            .unwrap_err()
            .code,
        "CampaignNotFound"
    );
    assert_eq!(
        call_login_campaign(&ctx, &mut sess, "claim_reward", &[])
            .await
            .unwrap_err()
            .code,
        "CampaignNotFound"
    );

    // 2. Undocked Omega login campaign claim -> delivers both Alpha + Omega rewards into redeem queue
    ctx.db.with_mut_snapshot(|snap| {
        snap.seasons_and_campaigns.login_campaign_enrollments.clear();
        let ch = snap.characters.get_mut(&cid).unwrap();
        ch.station_id = None;
        ch.structure_id = None;
        ch.clone_grade = 2; // Omega
    });
    sess.station_id = None;
    sess.structure_id = None;

    let claim_rep = call_login_campaign(&ctx, &mut sess, "claim_reward", &[])
        .await
        .unwrap();
    let PyRep::Tuple(parts) = claim_rep else {
        panic!("expected tuple from claim_reward");
    };
    let Some(PyRep::List(delivered_ids)) = parts[0].dict_get("delivered_item_ids") else {
        panic!("expected delivered_item_ids list");
    };
    assert_eq!(delivered_ids.len(), 2);

    // 3. Seasonal campaign: unbound character returns 4-tuple of None
    let mut unbound = sess.clone();
    unbound.character_id = None;
    let unbound_active = call_seasonal_campaign(&ctx, &mut unbound, "get_active_campaign", &[])
        .await
        .unwrap();
    assert_eq!(
        unbound_active,
        PyRep::Tuple(vec![PyRep::None, PyRep::None, PyRep::None, PyRep::None])
    );

    // No active seasonal schedule -> NoActiveCampaign on claim_reward, empty Dict on get_claim_history
    assert_eq!(
        call_seasonal_campaign(&ctx, &mut sess, "claim_reward", &[])
            .await
            .unwrap_err()
            .code,
        "NoActiveCampaign"
    );
    assert_eq!(
        call_seasonal_campaign(&ctx, &mut sess, "get_claim_history", &[])
            .await
            .unwrap(),
        PyRep::Dict(Vec::new())
    );

    // 4. Seasonal Alpha claim followed by same-day Omega upgrade triggers retroactive Omega reward delivery!
    let now = ctx.now_unix();
    let retro_camp_id = get_sde_login_campaigns(&ctx.sde)
        .values()
        .find(|c| c.retro_claim && !c.omega_rewards.is_empty())
        .map(|c| c.campaign_id)
        .unwrap_or(DEFAULT_SEASONAL_CAMPAIGN_ID);
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&cid).unwrap().clone_grade = 1; // Alpha first
        let mut ext = load_cosmetics_ui_state(snap);
        ext.active_seasonal_schedule = Some(SeasonalCampaignSchedule {
            campaign_id: retro_camp_id,
            start_unix: now.saturating_sub(86_400),
            end_unix: now + 10 * 86_400,
            turnover_secs: DEFAULT_SEASONAL_TURNOVER_SECS,
        });
        save_cosmetics_ui_state(snap, &ext);
    });

    assert_eq!(
        call_seasonal_campaign(&ctx, &mut sess, "claim_reward", &[])
            .await
            .unwrap(),
        PyRep::Bool(true)
    );

    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&cid).unwrap().clone_grade = 2;
    });
    assert_eq!(
        call_seasonal_campaign(&ctx, &mut sess, "claim_reward", &[])
            .await
            .unwrap(),
        PyRep::Bool(true)
    );
    assert_eq!(
        call_seasonal_campaign(&ctx, &mut sess, "claim_reward", &[])
            .await
            .unwrap_err()
            .code,
        "SeasonalRewardAlreadyClaimedToday"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn info_gathering_fifo_cap_milestones_and_community_tracks_edge_branches() {
    let ctx = test_ctx();
    let uid = 1_u64;
    let cid = 90_000_001_i64;
    let mut sess = auth_session(uid, cid, 98_000_001, 30_000_142);

    // 1. infoGatheringMgr string config parsing, disabled status code (1), and MAX_TELEMETRY_EVENTS cap
    {
        let mut g = ctx.global_config_values.write().unwrap();
        g.insert(
            INFO_GATHERING_ENABLED_CONFIG_KEY.into(),
            PyRep::String("0".into()),
        );
        g.insert(
            INFO_GATHERING_INTERVAL_CONFIG_KEY.into(),
            PyRep::String("45".into()),
        );
    }
    let cfg_off = call_info_gathering(&ctx, &mut sess, "GetStateAndConfig", &[])
        .await
        .unwrap();
    assert_eq!(cfg_off.dict_get("isEnabled"), Some(&PyRep::Bool(false)));
    assert_eq!(
        cfg_off.dict_get("clientWorkerInterval"),
        Some(&PyRep::Integer(45))
    );

    let disabled_log = call_info_gathering(
        &ctx,
        &mut sess,
        "LogInfoEventsFromClient",
        &[PyRep::Dict(vec![(PyRep::Integer(10), PyRep::Integer(1))])],
    )
    .await
    .unwrap();
    let PyRep::Dict(d_entries) = disabled_log else {
        panic!("expected dict");
    };
    assert_eq!(d_entries[0].1, PyRep::Integer(1));

    {
        let mut g = ctx.global_config_values.write().unwrap();
        g.insert(
            INFO_GATHERING_ENABLED_CONFIG_KEY.into(),
            PyRep::String("true".into()),
        );
    }
    ctx.db.with_mut_snapshot(|snap| {
        let mut ext = load_cosmetics_ui_state(snap);
        ext.client_telemetry_events = (0..MAX_TELEMETRY_EVENTS)
            .map(|i| format!("old_{i}"))
            .collect();
        save_cosmetics_ui_state(snap, &ext);
    });
    let _ = call_info_gathering(
        &ctx,
        &mut sess,
        "LogInfoEventsFromClient",
        &[PyRep::List(vec![
            PyRep::String("new_a".into()),
            PyRep::Bool(true),
        ])],
    )
    .await
    .unwrap();
    ctx.db.with_snapshot(|snap| {
        let ext = load_cosmetics_ui_state(snap);
        assert_eq!(ext.client_telemetry_events.len(), MAX_TELEMETRY_EVENTS);
        assert_eq!(ext.client_telemetry_events[0], "old_2");
    });

    assert_eq!(
        call_info_gathering(&ctx, &mut sess, "LogInfoEventsFromClient", &[])
            .await
            .unwrap_err()
            .code,
        "InvalidArgument"
    );
    assert_eq!(
        call_info_gathering(
            &ctx,
            &mut sess,
            "LogInfoEventsFromClient",
            &[PyRep::Integer(99)],
        )
        .await
        .unwrap_err()
        .code,
        "InvalidArgument"
    );

    // 2. milestoneMgr V24 120-minute milestone bounds, play-time accumulation, and 500,000 ISK payout
    assert_eq!(
        call_milestone_mgr(&ctx, &mut sess, "ClaimRewards", &[])
            .await
            .unwrap_err()
            .code,
        "InvalidArgument"
    );
    assert_eq!(
        call_milestone_mgr(&ctx, &mut sess, "ClaimRewards", &[PyRep::Integer(2)])
            .await
            .unwrap_err()
            .code,
        "InvalidMilestoneID"
    );

    let _ = call_milestone_mgr(&ctx, &mut sess, "ProcessCharacterLogon", &[])
        .await
        .unwrap();
    // Split play time across multiple sessions reaching 119m grace threshold
    ctx.advance_virtual_clock(3_600);
    let _ = call_milestone_mgr(&ctx, &mut sess, "ProcessCharacterLogon", &[])
        .await
        .unwrap();
    assert_eq!(
        call_milestone_mgr(&ctx, &mut sess, "ClaimRewards", &[PyRep::Integer(1)])
            .await
            .unwrap_err()
            .code,
        "MilestonePrerequisiteNotMet"
    );
    ctx.advance_virtual_clock(3_540);
    let bal0 = ctx.db.with_snapshot(|s| s.characters[&cid].balance.raw_cents());
    let _ = call_milestone_mgr(&ctx, &mut sess, "ClaimRewards", &[PyRep::Integer(1)])
        .await
        .unwrap();
    let bal2 = ctx.db.with_snapshot(|s| s.characters[&cid].balance.raw_cents());
    assert_eq!(bal2 - bal0, 50_000_000);

    assert_eq!(
        call_milestone_mgr(&ctx, &mut sess, "ProcessCharacterLogon", &[])
            .await
            .unwrap(),
        PyRep::None
    );

    // 3. communityTrackMgr negative ID, inactive track, progress clamping, and integer QA mode
    assert_eq!(
        call_community_track(&ctx, &mut sess, "get_instance_attributes_public", &[])
            .await
            .unwrap_err()
            .code,
        "InvalidArgument"
    );
    assert_eq!(
        call_community_track(
            &ctx,
            &mut sess,
            "get_instance_attributes_public",
            &[PyRep::Integer(-1)],
        )
        .await
        .unwrap_err()
        .code,
        "InvalidInstanceID"
    );
    assert_eq!(
        call_community_track(
            &ctx,
            &mut sess,
            "get_instance_attributes_public",
            &[PyRep::Integer(404)],
        )
        .await
        .unwrap_err()
        .code,
        "CommunityTrackInstanceNotFound"
    );

    let (&first_tid, first_tmpl) = get_sde_community_tracks(&ctx.sde)
        .iter()
        .next()
        .expect("SDE community track template");
    ctx.db.with_mut_snapshot(|snap| {
        let mut tracks = HashMap::new();
        tracks.insert(
            20_u32,
            crate::services::quasar_progression_extra::CommunityTrackState {
                track_id: 20,
                template_id: first_tid as u32,
                name: "OverGoal".into(),
                current_progress: u64::from(first_tmpl.goal) + 5_000,
                target_progress: u64::from(first_tmpl.goal),
                active: true,
                tale_templates: Vec::new(),
            },
        );
        snap.quasar_state_blobs.insert(
            "quasar_community_tracks".into(),
            serde_json::to_string(&tracks).unwrap(),
        );
    });
    let clamped_rep = call_community_track(
        &ctx,
        &mut sess,
        "get_instance_attributes_public",
        &[PyRep::Integer(20)],
    )
    .await
    .unwrap();
    assert_eq!(
        clamped_rep,
        PyRep::Tuple(vec![
            PyRep::Integer(first_tid),
            PyRep::Integer(i64::from(first_tmpl.goal)),
        ])
    );

    sess.role = service_roles::ROLE_PROGRAMMER;
    assert_eq!(
        call_community_track(&ctx, &mut sess, "change_qa_mode", &[])
            .await
            .unwrap_err()
            .code,
        "InvalidArgument"
    );
    assert_eq!(
        call_community_track(
            &ctx,
            &mut sess,
            "change_qa_mode",
            &[PyRep::String("bad".into())],
        )
        .await
        .unwrap_err()
        .code,
        "InvalidArgument"
    );
    assert_eq!(
        call_community_track(&ctx, &mut sess, "change_qa_mode", &[PyRep::Integer(0)])
            .await
            .unwrap(),
        PyRep::Bool(true)
    );
}

#[tokio::test]
async fn deliver_login_reward_docked_claim_notification_toggle_and_milestone_side_effects_kill_mutants() {
    let ctx = test_ctx();
    let cid1 = 90_000_001_i64;
    let uid1 = ctx.db.with_snapshot(|s| s.characters[&cid1].user_id);
    let station_id = 60_003_760_i64;
    let now = ctx.now_unix();

    // 1. Direct deliver_login_reward (docked + expiry + auto_injected + BPC, and undocked redeem token):
    //    kills `deliver_login_reward -> Default::default()`.
    let bpc_rw = SdeLoginRewardEntry {
        type_id: 587,
        quantity: 2,
        tier: 1,
        blueprint_runs: Some(5),
        blueprint_material_level: Some(10),
        blueprint_productivity_level: Some(20),
        redeemed_expiry_days: Some(7),
        is_auto_injected_booster_or_skin: true,
        overlay_texture_path: None,
    };
    ctx.db.with_mut_snapshot(|snap| {
        let docked = deliver_login_reward(snap, uid1, cid1, Some(station_id), 42, now, &bpc_rw);
        assert!(docked.primary_id > 0);
        let spawned = docked.spawned_item.expect("docked delivery must spawn item");
        assert_eq!(spawned.item_id, docked.primary_id);
        assert_eq!(spawned.type_id, 587);
        assert_eq!(spawned.location_id, station_id);
        assert_eq!(spawned.blueprint_runs_remaining, Some(5));
        assert!(docked.expire_date_time.is_some());
        assert!(snap.items.contains_key(&docked.primary_id));
        assert_eq!(
            snap.seasons_and_campaigns.login_reward_expiry.get(&docked.primary_id).copied(),
            docked.expire_date_time
        );
        assert!(snap.seasons_and_campaigns.auto_injected_rewards[&cid1].contains(&587));

        let undocked = deliver_login_reward(snap, uid1, cid1, None, 42, now, &bpc_rw);
        assert!(undocked.primary_id > 0);
        assert!(undocked.spawned_item.is_none());
        assert!(undocked.expire_date_time.is_some());
    });

    // 2. Docked loginCampaignManager.claim_reward with live_notify_listener = false vs true:
    //    kills `handle_login_campaign_claim_reward -> Ok(Default::default())` AND `delete !` on `!session.live_notify_listener`.
    ctx.db.with_mut_snapshot(|snap| {
        snap.seasons_and_campaigns = Default::default();
        if let Some(ch) = snap.characters.get_mut(&cid1) {
            ch.station_id = Some(station_id);
            ch.structure_id = None;
            ch.clone_grade = 2;
        }
    });
    let mut sess_false = auth_session(uid1, cid1, 98_000_001, 30_000_142);
    sess_false.station_id = Some(station_id);
    sess_false.live_notify_listener = false;
    let _ = sess_false.drain_notifications();
    let _ = ctx.drain_notifications_for_char(cid1);

    let claim1 = call_login_campaign(&ctx, &mut sess_false, "claim_reward", &[])
        .await
        .expect("docked claim_reward false listener");
    assert!(matches!(claim1, PyRep::Tuple(ref parts) if parts.len() == 4));
    let sess_false_notifs = sess_false.drain_notifications();
    assert!(
        sess_false_notifs.iter().any(|(ev, _)| ev == "OnItemChange"),
        "when !session.live_notify_listener, session must push OnItemChange"
    );

    let cid2 = 90_990_002_i64;
    ctx.db.provision_gm_account("login_rw_p2", "pw", cid2, "LoginRwP2");
    let uid2 = ctx.db.with_snapshot(|s| s.accounts["login_rw_p2"].user_id);
    ctx.db.acquire_online_lock(uid2, 2, Some(cid2), true, now).unwrap();
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&cid2) {
            ch.station_id = Some(station_id);
            ch.structure_id = None;
            ch.clone_grade = 1;
        }
    });
    let mut sess_true = auth_session(uid2, cid2, 98_000_001, 30_000_142);
    sess_true.station_id = Some(station_id);
    sess_true.live_notify_listener = true;
    let _ = sess_true.drain_notifications();
    let _ = ctx.drain_notifications_for_char(cid2);

    let claim2 = call_login_campaign(&ctx, &mut sess_true, "claim_reward", &[])
        .await
        .expect("docked claim_reward true listener");
    assert!(matches!(claim2, PyRep::Tuple(ref parts) if parts.len() == 4));
    let sess_true_notifs = sess_true.drain_notifications();
    assert!(
        sess_true_notifs.iter().all(|(ev, _)| ev != "OnItemChange"),
        "when session.live_notify_listener == true, session must not duplicate OnItemChange"
    );
    assert!(ctx
        .drain_notifications_for_char(cid2)
        .iter()
        .any(|n| n.event == "OnItemChange"));

    // 3. milestoneMgr.ClaimRewards persistence & replay error (kills `handle_milestone_claim_rewards -> Ok(Default::default())`).
    let _ = call_milestone_mgr(&ctx, &mut sess_false, "ProcessCharacterLogon", &[])
        .await
        .unwrap();
    ctx.advance_virtual_clock(7_200);
    let bal_before = ctx.db.with_snapshot(|s| s.characters[&cid1].balance.raw_cents());
    assert_eq!(
        call_milestone_mgr(&ctx, &mut sess_false, "ClaimRewards", &[PyRep::Integer(1)])
            .await
            .unwrap(),
        PyRep::None
    );
    let bal_after = ctx.db.with_snapshot(|s| s.characters[&cid1].balance.raw_cents());
    assert_eq!(bal_after - bal_before, 50_000_000);
    assert_eq!(
        call_milestone_mgr(&ctx, &mut sess_false, "ClaimRewards", &[PyRep::Integer(1)])
            .await
            .unwrap_err()
            .code,
        "MilestoneAlreadyClaimed"
    );
    ctx.db.with_snapshot(|snap| {
        let ext = load_cosmetics_ui_state(snap);
        assert!(ext.claimed_milestones.get(&cid1).is_some_and(|v| v.contains(&1)));
    });
}
