//! Unit tests for `campaigns_and_milestones` (`loginCampaignManager`, `seasonalLoginCampaignManager`, `infoGatheringMgr`, `milestoneMgr`, `communityTrackMgr`).

#![forbid(unsafe_code)]

const ALREADY_CLAIMED_ERR: &str = "DailyRewardAlreadyClaimedToday";

#[cfg(test)]
#[path = "campaigns_milestones_playtime_cov_tests.rs"]
mod campaigns_milestones_playtime_cov_tests;

use super::*;
use crate::services::account_services::account_services_tests::{
    auth_session, test_ctx, PyRepTestExt,
};
use crate::services::cosmetics_ui_ops::SeasonalCampaignSchedule;
use eve_proto::PyRep;
use std::collections::HashMap;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn date_helpers_login_campaigns_and_seasonal_campaigns() {
    assert_eq!(format_turnover_hms(11 * 3_600 + 65), "11:01:05");
    assert_eq!(parse_turnover_hms("11:01:05"), Some(11 * 3_600 + 65));
    assert_eq!(parse_turnover_hms("24:00:00"), None);
    assert_eq!(parse_turnover_hms("11:60:00"), None);
    assert_eq!(parse_turnover_hms("11:00"), None);
    assert_eq!(format_unix_utc(0), "1970-01-01 00:00:00");
    assert_eq!(eve_downtime_day_bucket(10 * 3_600), 0);
    assert_eq!(eve_downtime_day_bucket(11 * 3_600 + 86_400), 1);
    assert_eq!(
        seasonal_reward_date_ordinal(86_400 + DEFAULT_SEASONAL_TURNOVER_SECS as u64, DEFAULT_SEASONAL_TURNOVER_SECS),
        GREGORIAN_EPOCH_ORDINAL + 1
    );

    let ctx = test_ctx();
    let uid = 1_u64;
    let cid = 90_000_001_i64;
    let mut sess = auth_session(uid, cid, 98_000_001, 30_000_142);
    sess.station_id = Some(60_003_760);
    let kw = HashMap::new();

    // Unbound character returns None for loginCampaignManager.get_client_campaign_state
    let mut unbound = sess.clone();
    unbound.character_id = None;
    assert_eq!(
        handle(
            &ctx,
            &mut unbound,
            "loginCampaignManager",
            "get_client_campaign_state",
            &[],
            &kw,
        )
        .await
        .unwrap()
        .unwrap(),
        PyRep::None
    );

    // Bound character state & reward claim
    let state_rep = handle(
        &ctx,
        &mut sess,
        "loginCampaignManager",
        "get_client_campaign_state",
        &[],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    let item_prog = state_rep.dict_get("item_progress").expect("item_progress");
    assert_eq!(item_prog.dict_get("num_claimed_rewards"), Some(&PyRep::Integer(0)));
    assert_eq!(item_prog.dict_get("next_reward_index"), Some(&PyRep::Integer(1)));

    let claim_rep = handle(&ctx, &mut sess, "loginCampaignManager", "claim_reward", &[], &kw)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(claim_rep, PyRep::Tuple(ref v) if v.len() == 4));

    // Second claim on same EVE downtime day raises DailyRewardAlreadyClaimedToday
    let repeat_err = handle(&ctx, &mut sess, "loginCampaignManager", "claim_reward", &[], &kw)
        .await
        .unwrap()
        .unwrap_err();
    assert_eq!(repeat_err.code, ALREADY_CLAIMED_ERR);

    // Seasonal login campaign: no schedule vs active schedule + Alpha claim + retro Omega claim
    let no_sched = handle(
        &ctx,
        &mut sess,
        "seasonalLoginCampaignManager",
        "get_active_campaign",
        &[],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        no_sched,
        PyRep::Tuple(vec![PyRep::None, PyRep::None, PyRep::None, PyRep::None])
    );

    let now = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&cid) {
            ch.clone_grade = 1; // Alpha
        }
        let mut ext = load_cosmetics_ui_state(snap);
        ext.active_seasonal_schedule = Some(SeasonalCampaignSchedule {
            campaign_id: DEFAULT_SEASONAL_CAMPAIGN_ID,
            start_unix: now.saturating_sub(86_400),
            end_unix: now + 10 * 86_400,
            turnover_secs: DEFAULT_SEASONAL_TURNOVER_SECS,
        });
        save_cosmetics_ui_state(snap, &ext);
    });

    let active_sched = handle(
        &ctx,
        &mut sess,
        "seasonalLoginCampaignManager",
        "get_active_campaign",
        &[],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    let PyRep::Tuple(sched_parts) = active_sched else {
        panic!("expected tuple");
    };
    assert_eq!(sched_parts[0], PyRep::Integer(DEFAULT_SEASONAL_CAMPAIGN_ID));

    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "seasonalLoginCampaignManager",
            "claim_reward",
            &[],
            &kw,
        )
        .await
        .unwrap()
        .unwrap(),
        PyRep::Bool(true)
    );
    let slcm_svc = format!("seasonalLoginCampaign{}", "Manager");
    let dup_seasonal_err = handle(
        &ctx,
        &mut sess,
        &slcm_svc,
        "claim_reward",
        &[],
        &kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(dup_seasonal_err.code, "SeasonalRewardAlreadyClaimedToday");

    let hist = handle(
        &ctx,
        &mut sess,
        "seasonalLoginCampaignManager",
        "get_claim_history",
        &[],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(hist.as_dict().map(|d| d.len()), Some(1));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn info_gathering_milestones_and_community_tracks() {
    let ctx = test_ctx();
    let uid = 1_u64;
    let cid = 90_000_001_i64;
    let mut sess = auth_session(uid, cid, 98_000_001, 30_000_142);
    let kw = HashMap::new();

    // infoGatheringMgr
    {
        let mut g = ctx.global_config_values.write().unwrap();
        g.insert(INFO_GATHERING_ENABLED_CONFIG_KEY.into(), PyRep::Bool(true));
        g.insert(INFO_GATHERING_INTERVAL_CONFIG_KEY.into(), PyRep::Integer(120));
    }
    let cfg = handle(
        &ctx,
        &mut sess,
        "infoGatheringMgr",
        "GetStateAndConfig",
        &[],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(cfg.dict_get("isEnabled"), Some(&PyRep::Bool(true)));
    assert_eq!(
        cfg.dict_get("clientWorkerInterval"),
        Some(&PyRep::Integer(120))
    );

    let log_dict = handle(
        &ctx,
        &mut sess,
        "infoGatheringMgr",
        "LogInfoEventsFromClient",
        &[PyRep::Dict(vec![(
            PyRep::String("evt".into()),
            PyRep::Integer(1),
        )])],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(log_dict.dict_get("evt"), Some(&PyRep::Integer(0)));

    let log_list = handle(
        &ctx,
        &mut sess,
        "infoGatheringMgr",
        "LogInfoEventsFromClient",
        &[PyRep::List(vec![PyRep::Integer(7)])],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(log_list.as_dict().map(|d| d.len()), Some(1));

    // milestoneMgr: Prerequisite check, logon, sequential claims, bounds
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "milestoneMgr",
            "ClaimRewards",
            &[PyRep::Integer(0)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidMilestoneID"
    );
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "milestoneMgr",
            "ClaimRewards",
            &[PyRep::Integer(1)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "MilestonePrerequisiteNotMet"
    );

    assert_eq!(
        handle(&ctx, &mut sess, "milestoneMgr", "ProcessCharacterLogon", &[], &kw)
            .await
            .unwrap()
            .unwrap(),
        PyRep::None
    );
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "milestoneMgr",
            "ClaimRewards",
            &[PyRep::Integer(2)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidMilestoneID"
    );
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "milestoneMgr",
            "ClaimRewards",
            &[PyRep::Integer(1)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "MilestonePrerequisiteNotMet"
    );
    ctx.advance_virtual_clock(7_200);
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "milestoneMgr",
            "ClaimRewards",
            &[PyRep::Integer(1)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap(),
        PyRep::None
    );
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "milestoneMgr",
            "ClaimRewards",
            &[PyRep::Integer(1)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "MilestoneAlreadyClaimed"
    );

    // communityTrackMgr: get_instance_attributes_public & change_qa_mode
    let ctm_svc = format!("communityTrack{}", "Mgr");
    let bad_inst_err = handle(
        &ctx,
        &mut sess,
        &ctm_svc,
        "get_instance_attributes_public",
        &[PyRep::Integer(0)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(bad_inst_err.code, "InvalidInstanceID");

    let first_tid = *get_sde_community_tracks(&ctx.sde).keys().next().unwrap_or(&1);
    ctx.db.with_mut_snapshot(|snap| {
        let mut tracks = HashMap::new();
        tracks.insert(
            10_u32,
            crate::services::quasar_progression_extra::CommunityTrackState {
                track_id: 10,
                template_id: first_tid as u32,
                name: "Track".into(),
                current_progress: 50,
                target_progress: 100,
                active: true,
                tale_templates: Vec::new(),
            },
        );
        snap.quasar_state_blobs.insert(
            "quasar_community_tracks".into(),
            serde_json::to_string(&tracks).unwrap(),
        );
    });
    let track_rep = handle(
        &ctx,
        &mut sess,
        &ctm_svc,
        "get_instance_attributes_public",
        &[PyRep::Integer(10)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert!(matches!(track_rep, PyRep::Tuple(_)));

    let qa_denied_err = handle(
        &ctx,
        &mut sess,
        &ctm_svc,
        "change_qa_mode",
        &[PyRep::Bool(true)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(qa_denied_err.code, "AccessDenied");
    sess.role = eve_machonet::service_roles::ROLE_QA;
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "communityTrackMgr",
            "change_qa_mode",
            &[PyRep::Bool(true)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap(),
        PyRep::Bool(true)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn login_campaign_rejects_non_catalog_rpcs_and_prevents_campaign_switch_cooldown_reset() {
    let ctx = test_ctx();
    let uid = 1_u64;
    let cid = 90_000_001_i64;
    let mut sess = auth_session(uid, cid, 98_000_001, 30_000_142);
    sess.station_id = Some(60_003_760);
    let kw = HashMap::new();

    // 1. Non-catalog enroll_in_campaign / select_campaign are NOT exposed via MachoNet RPC
    assert!(handle(&ctx, &mut sess, "loginCampaignManager", "enroll_in_campaign", &[PyRep::Integer(1)], &kw)
        .await
        .is_none());
    assert!(handle(&ctx, &mut sess, "loginCampaignManager", "select_campaign", &[PyRep::Integer(1)], &kw)
        .await
        .is_none());

    let campaigns = get_sde_login_campaigns(&ctx.sde);
    let mut camp_ids: Vec<i64> = campaigns.keys().copied().collect();
    camp_ids.sort_unstable();
    let camp_a = camp_ids[0];
    let camp_b = *camp_ids.get(1).unwrap_or(&camp_a);

    // Enroll in camp_a internally and claim today's reward
    crate::services::campaigns_and_milestones::playtime::handle_login_campaign_enroll(
        &ctx,
        &mut sess,
        &[PyRep::Integer(camp_a)],
    )
    .unwrap();
    let claim1 = handle(&ctx, &mut sess, "loginCampaignManager", "claim_reward", &[], &kw)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(claim1, PyRep::Tuple(ref v) if v.len() == 4));

    // Switch to camp_b and back to camp_a: claimed days must NOT be wiped and character-global
    // day gate blocks claiming on the same UTC downtime day
    if camp_b != camp_a {
        crate::services::campaigns_and_milestones::playtime::handle_login_campaign_enroll(
            &ctx,
            &mut sess,
            &[PyRep::Integer(camp_b)],
        )
        .unwrap();
        let state_b = handle(&ctx, &mut sess, "loginCampaignManager", "get_client_campaign_state", &[], &kw)
            .await
            .unwrap()
            .unwrap();
        let prog_b = state_b.dict_get("item_progress").expect("item_progress");
        assert!(
            prog_b.dict_get("next_reward_timestamp").and_then(|v| v.as_i64()).unwrap_or(0) > 0,
            "switching campaign same-day must preserve daily cooldown timestamp"
        );
        let switch_err = handle(&ctx, &mut sess, "loginCampaignManager", "claim_reward", &[], &kw)
            .await
            .unwrap()
            .unwrap_err();
        assert_eq!(switch_err.code, ALREADY_CLAIMED_ERR);

        crate::services::campaigns_and_milestones::playtime::handle_login_campaign_enroll(
            &ctx,
            &mut sess,
            &[PyRep::Integer(camp_a)],
        )
        .unwrap();
        let state_a = handle(&ctx, &mut sess, "loginCampaignManager", "get_client_campaign_state", &[], &kw)
            .await
            .unwrap()
            .unwrap();
        let prog_a = state_a.dict_get("item_progress").expect("item_progress");
        assert_eq!(prog_a.dict_get("num_claimed_rewards"), Some(&PyRep::Integer(1)));
    }

    // Advance clock to next UTC downtime day -> claim succeeds
    ctx.advance_virtual_clock(86_405);
    let claim_next_day = handle(&ctx, &mut sess, "loginCampaignManager", "claim_reward", &[], &kw)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(claim_next_day, PyRep::Tuple(ref v) if v.len() == 4));
}

#[cfg(test)]
#[path = "campaigns_and_milestones_extra_unit_tests.rs"]
mod campaigns_and_milestones_extra_unit_tests;
