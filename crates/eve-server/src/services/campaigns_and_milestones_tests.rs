//! Unit tests for `campaigns_and_milestones` (`loginCampaignManager`, `seasonalLoginCampaignManager`, `infoGatheringMgr`, `milestoneMgr`, `communityTrackMgr`).

#![forbid(unsafe_code)]

use super::*;
use crate::services::account_services::account_services_tests::{
    auth_session, test_ctx, PyRepTestExt,
};
use eve_proto::PyRep;
use std::collections::HashMap;

#[tokio::test]
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
    assert_eq!(
        state_rep.dict_get("canClaimToday"),
        Some(&PyRep::Bool(true))
    );

    let claim_rep = handle(&ctx, &mut sess, "loginCampaignManager", "claim_reward", &[], &kw)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(claim_rep, PyRep::Tuple(_)));

    // Second claim on same day returns PyRep::None
    assert_eq!(
        handle(&ctx, &mut sess, "loginCampaignManager", "claim_reward", &[], &kw)
            .await
            .unwrap()
            .unwrap(),
        PyRep::None
    );

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

#[tokio::test]
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
        "MilestonePrerequisiteNotMet"
    );
    ctx.db.clock().advance_secs(7_200);
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
