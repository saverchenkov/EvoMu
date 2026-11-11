//! Direct unit tests covering all branches of `campaigns_milestones_playtime`.

#![forbid(unsafe_code)]

use super::super::playtime::*;
use super::super::*;
use crate::services::account_services::account_services_tests::{auth_session, test_ctx};
use crate::services::cosmetics_ui_ops::{
    load_cosmetics_ui_state, save_cosmetics_ui_state, SeasonalCampaignSchedule,
};
use eve_proto::PyRep;
use std::collections::{BTreeMap, HashMap};

fn make_test_campaign(cid: i64, name_id: u32, days: u32, retro: bool) -> SdeLoginCampaign {
    let mut alpha = BTreeMap::new();
    for d in 0..days {
        alpha.insert(
            d,
            SdeLoginRewardEntry {
                type_id: 34,
                quantity: 10,
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
    SdeLoginCampaign {
        campaign_id: cid,
        campaign_name_id: name_id,
        auto_claim: false,
        retro_claim: retro,
        window_header_background: None,
        alpha_rewards: alpha,
        omega_rewards: BTreeMap::new(),
    }
}

#[test]
fn milestone_playtime_state_logon_pause_and_offline_gap_exclusion() {
    let st_off = CharacterMilestoneState {
        first_logon_unix: 100,
        last_activity_unix: 200,
        accumulated_playtime_secs: 500,
        active_session: false,
    };
    assert_eq!(st_off.accumulated_playtime_now(9_999), 500);

    let st_zero = CharacterMilestoneState {
        first_logon_unix: 100,
        last_activity_unix: 0,
        accumulated_playtime_secs: 250,
        active_session: true,
    };
    assert_eq!(st_zero.accumulated_playtime_now(9_999), 250);

    let st_on = CharacterMilestoneState {
        first_logon_unix: 100,
        last_activity_unix: 1_000,
        accumulated_playtime_secs: 250,
        active_session: true,
    };
    assert_eq!(st_on.accumulated_playtime_now(1_300), 550);
    assert_eq!(st_on.accumulated_playtime_now(500), 250);

    let ctx = test_ctx();
    let cid = 90_000_001_i64;

    ctx.db.with_mut_snapshot(|snap| {
        // Malformed JSON -> None
        snap.seasons_and_campaigns.milestone_playtime.remove(&cid);
        assert!(load_milestone_state(snap, cid).is_none());

        // Pause when missing -> no-op
        pause_milestone_playtime_in_snap(snap, cid, 1_000);
        assert!(load_milestone_state(snap, cid).is_none());

        // First logon sets first_logon_unix and active_session = true
        let s1 = record_milestone_logon(snap, cid, 1_000);
        assert_eq!(s1.first_logon_unix, 1_000);
        assert!(s1.active_session);
        assert_eq!(s1.accumulated_playtime_secs, 0);

        // Second logon while still active adds elapsed 300s
        let s2 = record_milestone_logon(snap, cid, 1_300);
        assert_eq!(s2.accumulated_playtime_secs, 300);
        assert_eq!(s2.last_activity_unix, 1_300);

        // Pause flushes 200s -> 500s total and sets active_session = false
        pause_milestone_playtime_in_snap(snap, cid, 1_500);
        let paused = load_milestone_state(snap, cid).unwrap();
        assert!(!paused.active_session);
        assert_eq!(paused.accumulated_playtime_secs, 500);

        // Pause again while already inactive is a no-op
        pause_milestone_playtime_in_snap(snap, cid, 9_000);
        assert_eq!(load_milestone_state(snap, cid).unwrap().accumulated_playtime_secs, 500);

        // Logon after 10,000s offline gap must NOT count offline time
        let s3 = record_milestone_logon(snap, cid, 11_500);
        assert_eq!(s3.accumulated_playtime_secs, 500);
        assert!(s3.active_session);

        // Force first_logon_unix == 0 and last_activity_unix == 0, then logon + pause
        let mut zero_st = s3;
        zero_st.first_logon_unix = 0;
        zero_st.last_activity_unix = 0;
        save_milestone_state(snap, cid, &zero_st);
        let s4 = record_milestone_logon(snap, cid, 12_000);
        assert_eq!(s4.first_logon_unix, 12_000);
        assert_eq!(s4.accumulated_playtime_secs, 500);
    });

    // External WorldRepository wrapper pause_milestone_playtime
    pause_milestone_playtime(&ctx.db, cid, 12_600);
    ctx.db.with_snapshot(|snap| {
        let st = load_milestone_state(snap, cid).unwrap();
        assert!(!st.active_session);
        assert_eq!(st.accumulated_playtime_secs, 1_100);
    });
}

#[test]
fn resolve_daily_campaign_enroll_seasonal_schedule_labels_notifications_and_station() {
    let ctx = test_ctx();
    let cid = 90_000_001_i64;
    let other_cid = 90_000_002_i64;
    let mut sess = auth_session(1, cid, 98_000_001, 30_000_142);

    // 1. Empty campaigns map -> CampaignNotFound
    ctx.db.with_snapshot(|snap| {
        let empty = BTreeMap::new();
        assert_eq!(
            resolve_daily_campaign(snap, cid, 1_000, &empty)
                .unwrap_err()
                .code,
            "CampaignNotFound"
        );
    });

    // 2. Scheduled campaign window + is_rookie override + expiry + ext fallback + evergreen selection
    let mut campaigns = BTreeMap::new();
    campaigns.insert(10, make_test_campaign(10, 111, 14, false));
    campaigns.insert(20, make_test_campaign(20, 222, 30, true));
    campaigns.insert(30, make_test_campaign(30, 333, 30, false));

    ctx.db.with_mut_snapshot(|snap| {
        // Evergreen fallback picks non-retro 30+ day track (30)
        let (picked, is_rookie, _) = resolve_daily_campaign(snap, cid, 1_500, &campaigns).unwrap();
        assert_eq!(picked, 30);
        assert!(!is_rookie);

        // ext.active_login_campaign_id selects 10 (<=14 days => is_rookie = true)
        let mut ext = load_cosmetics_ui_state(snap);
        ext.active_login_campaign_id = Some(10);
        save_cosmetics_ui_state(snap, &ext);
        let (picked_ext, rook_ext, _) = resolve_daily_campaign(snap, cid, 1_500, &campaigns).unwrap();
        assert_eq!(picked_ext, 10);
        assert!(rook_ext);

        // Active scheduled campaign overrides ext.active_login_campaign_id and honors explicit is_rookie = Some(false)
        snap.seasons_and_campaigns.daily_login_campaign_schedule = Some(ScheduledDailyCampaign {
                campaign_id: 20,
                start_unix: 1_000,
                end_unix: Some(2_000),
                is_rookie: Some(false),
            });
        let (picked_sched, rook_sched, _) = resolve_daily_campaign(snap, cid, 1_500, &campaigns).unwrap();
        assert_eq!(picked_sched, 20);
        assert!(!rook_sched);

        // Outside schedule window (now >= end_unix) falls back to ext (10)
        let (picked_expired, _, _) = resolve_daily_campaign(snap, cid, 2_000, &campaigns).unwrap();
        assert_eq!(picked_expired, 10);
    });

    // Only short/retro campaigns fallback when no 30+ non-retro exists
    let mut short_only = BTreeMap::new();
    short_only.insert(5, make_test_campaign(5, 55, 7, true));
    ctx.db.with_mut_snapshot(|snap| {
        let mut ext = load_cosmetics_ui_state(snap);
        ext.active_login_campaign_id = None;
        save_cosmetics_ui_state(snap, &ext);
        snap.seasons_and_campaigns.daily_login_campaign_schedule = None;
        let (picked_short, rook_short, _) = resolve_daily_campaign(snap, cid, 1_500, &short_only).unwrap();
        assert_eq!(picked_short, 5);
        assert!(rook_short);
    });

    // 3. handle_login_campaign_enroll validation & per-character isolation
    let mut unbound = sess.clone();
    unbound.character_id = None;
    assert!(handle_login_campaign_enroll(&ctx, &mut unbound, &[PyRep::Integer(1)]).is_err());
    assert_eq!(
        handle_login_campaign_enroll(&ctx, &mut sess, &[]).unwrap_err().code,
        "InvalidArgument"
    );
    assert_eq!(
        handle_login_campaign_enroll(&ctx, &mut sess, &[PyRep::Integer(999_999_999)])
            .unwrap_err()
            .code,
        "CampaignNotFound"
    );

    let real_cids: Vec<i64> = get_sde_login_campaigns(&ctx.sde).keys().copied().collect();
    if let Some(&first_cid) = real_cids.first() {
        ctx.db.with_mut_snapshot(|snap| {
            let mut m = HashMap::new();
            m.insert(other_cid, first_cid);
            snap.seasons_and_campaigns.login_campaign_enrollments = m;
        });
        assert_eq!(
            handle_login_campaign_enroll(&ctx, &mut sess, &[PyRep::Integer(first_cid)]).unwrap(),
            PyRep::Bool(true)
        );
        ctx.db.with_snapshot(|snap| {
            let m = &snap.seasons_and_campaigns.login_campaign_enrollments;
            assert_eq!(m.get(&cid), Some(&first_cid));
            assert_eq!(m.get(&other_cid), Some(&first_cid));
        });
    }

    // 4. active_seasonal_schedule_from_snapshot: ext vs blob fallback vs expired
    ctx.db.with_mut_snapshot(|snap| {
        let mut ext = load_cosmetics_ui_state(snap);
        ext.active_seasonal_schedule = Some(SeasonalCampaignSchedule {
            campaign_id: 100,
            start_unix: 100,
            end_unix: 200,
            turnover_secs: 39_600,
        });
        snap.seasons_and_campaigns.seasonal_login_schedule = Some(eve_db::SeasonalCampaignSchedule {
            campaign_id: 200,
            start_unix: 150,
            end_unix: 500,
            turnover_secs: 39_600,
        });
        assert_eq!(
            active_seasonal_schedule_from_snapshot(snap, &ext, 150)
                .map(|s| s.campaign_id),
            Some(100)
        );
        assert_eq!(
            active_seasonal_schedule_from_snapshot(snap, &ext, 250)
                .map(|s| s.campaign_id),
            Some(200)
        );
        assert!(active_seasonal_schedule_from_snapshot(snap, &ext, 500).is_none());
    });

    // 5. resolve_reward_label_message_id, emit_account_change_isk, resolve_reward_station_id
    let dummy_camp = make_test_campaign(77, 424_242, 7, false);
    assert_eq!(resolve_reward_label_message_id(&ctx, 999_999_888, &dummy_camp), 424_242);
    if let Some((&tid, _)) = ctx.sde.types.iter().next() {
        let lbl = resolve_reward_label_message_id(&ctx, tid, &dummy_camp);
        assert!(lbl > 0);
    }

    sess.drain_notifications();
    emit_account_change_isk(&ctx, &mut sess, cid, 123_456);
    let notes = sess.drain_notifications();
    assert_eq!(notes.len(), 1);
    assert_eq!(notes[0].0, "OnAccountChange");
    assert_eq!(notes[0].1[0], PyRep::String("cash".into()));
    assert_eq!(notes[0].1[1], PyRep::Integer(cid));
    assert_eq!(notes[0].1[2], PyRep::Real(1234.56));

    let mut ch = ctx.db.with_snapshot(|s| s.characters[&cid].clone());
    ch.station_id = None;
    ch.structure_id = None;
    assert_eq!(resolve_reward_station_id(Some(60_003_760), &ch), Some(60_003_760));
    ch.station_id = Some(60_003_761);
    ch.structure_id = Some(1_020_000_001);
    assert_eq!(resolve_reward_station_id(Some(0), &ch), Some(60_003_761));
    ch.station_id = Some(0);
    assert_eq!(resolve_reward_station_id(None, &ch), Some(1_020_000_001));
    ch.structure_id = None;
    assert_eq!(resolve_reward_station_id(None, &ch), None);
}
