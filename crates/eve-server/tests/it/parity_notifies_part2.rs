//! Runtime integration tests for open `notify:*` parity rows (Part 2).

#![forbid(unsafe_code)]

use crate::common;
use eve_machonet::{service_roles::ROLE_GM_DEFAULT, SessionState};
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::{NotificationTarget, ServerContext};
use eve_server::services::{
    notify_emitters_batch3, notify_emitters_batch9, notify_emitters_missing,
};
use std::collections::HashMap;

const PILOT_A: i64 = 90_000_001;

fn new_ctx() -> ServerContext {
    let sde = std::sync::Arc::new(
        eve_sde::SdeRegistry::open(&eve_sde::SdeRegistry::default_db_path()).expect("open sde"),
    );
    let db = std::sync::Arc::new(eve_db::WorldRepository::with_demo_seed());
    let ctx = ServerContext::with_sde_and_repository(sde, db);
    ctx.set_clock_paused(true);
    ctx
}

async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    svc: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    ctx.dispatch_rpc(sess, svc, None, method, args, &HashMap::new(), None)
        .await
}

fn drain_all(ctx: &ServerContext, sess: &mut SessionState, char_id: i64) -> Vec<(String, Vec<PyRep>)> {
    let mut out = sess.drain_notifications();
    for n in ctx.drain_notifications_for_char(char_id) {
        out.push((n.event, n.args));
    }
    out
}

fn find_notif<'a>(notifs: &'a [(String, Vec<PyRep>)], event: &str) -> &'a [PyRep] {
    notifs
        .iter()
        .find(|(ev, _)| ev == event)
        .map(|(_, args)| args.as_slice())
        .unwrap_or_else(|| {
            let seen: Vec<&str> = notifs.iter().map(|(e, _)| e.as_str()).collect();
            panic!("expected notification {event}, got {seen:?}")
        })
}

async fn setup_pilot(ctx: &ServerContext, sid: u64) -> SessionState {
    let mut sess = SessionState::new(sid);
    common::login_pilot(ctx, &mut sess, "demo", "demo-secret").await;
    rpc(
        ctx,
        &mut sess,
        "charUnboundMgr",
        "SelectCharacterID",
        &[PyRep::Integer(PILOT_A)],
    )
    .await
    .expect("SelectCharacterID");
    ctx.advance_secs(12);
    let _ = drain_all(ctx, &mut sess, PILOT_A);
    sess
}

#[tokio::test]
async fn test_parity_notifies_seasons_campaigns_and_skills() {
    // parity: notify:OnCCPNotification, notify:OnContextualOfferPurchased, notify:OnLoginMilestoneAchieved
    // parity: notify:OnSeasonOmegaRewardsClaimed, notify:OnSeasonalGoalCompleted, notify:OnSeasonalGoalsReset
    // parity: notify:OnSkillBundleInjected
    eve_testkit::attest(&[
        "notify:OnCCPNotification",
        "notify:OnContextualOfferPurchased",
        "notify:OnLoginMilestoneAchieved",
        "notify:OnSeasonOmegaRewardsClaimed",
        "notify:OnSeasonalGoalCompleted",
        "notify:OnSeasonalGoalsReset",
        "notify:OnSkillBundleInjected",
    ]);
    let ctx = new_ctx();
    let mut sess = setup_pilot(&ctx, 1201).await;
    sess.role = ROLE_GM_DEFAULT;

    notify_emitters_batch3::emit_ccp_notification(
        &ctx,
        NotificationTarget::Character(PILOT_A),
        "Cluster Notice",
        "Maintenance",
        "Scheduled downtime at 11:00 UTC",
        101,
        55,
    );
    let ccp_n = drain_all(&ctx, &mut sess, PILOT_A);
    assert_eq!(find_notif(&ccp_n, "OnCCPNotification")[0], PyRep::String("Cluster Notice".into()));

    notify_emitters_batch3::emit_contextual_offer_purchased(&ctx, PILOT_A, 3301);
    let offer_n = drain_all(&ctx, &mut sess, PILOT_A);
    assert_eq!(find_notif(&offer_n, "OnContextualOfferPurchased")[0], PyRep::Integer(3301));

    notify_emitters_batch3::emit_login_milestone_achieved(&ctx, PILOT_A, 7, 1_500_000.0, true);
    let mile_n = drain_all(&ctx, &mut sess, PILOT_A);
    assert_eq!(
        find_notif(&mile_n, "OnLoginMilestoneAchieved"),
        &[PyRep::Integer(7), PyRep::Real(1_500_000.0), PyRep::Bool(true)]
    );

    let mut target_ch_id = 1_005_i64;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&PILOT_A) {
            ch.clone_grade = 2;
        }
        if let Some(mut def) =
            eve_server::services::season_fsd::default_seasonal_challenge_definitions()
                .into_iter()
                .next()
        {
            target_ch_id = def.challenge_id;
            def.client_event_driven = true;
            def.event_conditions.clear();
            def.max_progress = 2;
            def.points = 20;
            snap.service_state_blobs.insert(
                eve_server::services::season_fsd::SEASONAL_CHALLENGES_CONFIG_KEY.into(),
                serde_json::to_string(&vec![def]).unwrap(),
            );
        }
    });
    let _ = rpc(&ctx, &mut sess, "seasonManager", "fill_up_and_return_challenges_for_character", &[])
        .await
        .expect("fill challenges");
    for _ in 0..2 {
        rpc(
            &ctx,
            &mut sess,
            "seasonManager",
            "process_client_event",
            &[PyRep::Integer(target_ch_id)],
        )
        .await
        .expect("complete challenge");
    }
    let _ = drain_all(&ctx, &mut sess, PILOT_A);
    let claim = rpc(&ctx, &mut sess, "seasonManager", "claim_challenge_rewards", &[PyRep::Integer(target_ch_id)])
        .await
        .expect("claim_challenge_rewards");
    assert_eq!(claim.dict_get("claimed").and_then(|v| v.as_bool()), Some(true));
    let claim_n = drain_all(&ctx, &mut sess, PILOT_A);
    assert_eq!(find_notif(&claim_n, "OnSeasonalGoalCompleted")[0], PyRep::Integer(0));
    assert_eq!(
        find_notif(&claim_n, "OnSeasonOmegaRewardsClaimed"),
        &[PyRep::List(vec![PyRep::Integer(0), PyRep::Integer(1)])]
    );

    rpc(&ctx, &mut sess, "slash", "SlashCmd", &[PyRep::String(format!("/season reset_goals {PILOT_A}"))])
        .await
        .expect("/season reset_goals");
    let reset_n = drain_all(&ctx, &mut sess, PILOT_A);
    assert_eq!(find_notif(&reset_n, "OnSeasonalGoalsReset").len(), 0);

    notify_emitters_batch9::emit_skill_bundle_injected(&ctx, PILOT_A, 3, 75_000);
    let skill_n = drain_all(&ctx, &mut sess, PILOT_A);
    assert_eq!(
        find_notif(&skill_n, "OnSkillBundleInjected"),
        &[PyRep::Integer(3), PyRep::Integer(75_000)]
    );
}

#[tokio::test]
async fn test_parity_notifies_missing_client_and_economy() {
    // parity: notify:OnBeanPrime, notify:OnClearFetchedData, notify:OnClientEvent_MarketItemReceived
    // parity: notify:OnEffectMessage, notify:OnFlaggedListsChange, notify:OnNotificationUndeleted
    // parity: notify:OnOfferReceived, notify:OnPlexDepositFailed
    eve_testkit::attest(&[
        "notify:OnBeanPrime",
        "notify:OnClearFetchedData",
        "notify:OnClientEvent_MarketItemReceived",
        "notify:OnEffectMessage",
        "notify:OnFlaggedListsChange",
        "notify:OnNotificationUndeleted",
        "notify:OnOfferReceived",
        "notify:OnPlexDepositFailed",
    ]);
    let ctx = new_ctx();
    let mut sess = setup_pilot(&ctx, 1202).await;

    notify_emitters_missing::emit_bean_prime(&ctx, PILOT_A, &[(101, 2, 133_000_000_000_000_000)]);
    let bean_n = drain_all(&ctx, &mut sess, PILOT_A);
    assert_eq!(find_notif(&bean_n, "OnBeanPrime").len(), 1);

    notify_emitters_missing::emit_clear_fetched_data(&ctx, PILOT_A);
    let clear_n = drain_all(&ctx, &mut sess, PILOT_A);
    assert_eq!(find_notif(&clear_n, "OnClearFetchedData").len(), 0);

    notify_emitters_missing::emit_client_market_item_received(&ctx, PILOT_A, 88_001, 34, 4, 60_003_760);
    let mkt_n = drain_all(&ctx, &mut sess, PILOT_A);
    assert_eq!(
        find_notif(&mkt_n, "OnClientEvent_MarketItemReceived"),
        &[PyRep::Integer(88_001), PyRep::Integer(34), PyRep::Integer(4), PyRep::Integer(60_003_760)]
    );

    notify_emitters_missing::emit_effect_message(
        &ctx,
        PILOT_A,
        "CannotActivateCloakWhileTargeted",
        &[("targetCount", PyRep::Integer(1))],
    );
    let eff_n = drain_all(&ctx, &mut sess, PILOT_A);
    assert_eq!(find_notif(&eff_n, "OnEffectMessage")[0], PyRep::String("CannotActivateCloakWhileTargeted".into()));

    notify_emitters_missing::emit_flagged_lists_change(&ctx, PILOT_A);
    let flag_n = drain_all(&ctx, &mut sess, PILOT_A);
    assert_eq!(find_notif(&flag_n, "OnFlaggedListsChange").len(), 0);

    let undel_res = notify_emitters_missing::handle_undelete_notifications(
        &ctx,
        PILOT_A,
        &[501, 502],
    )
    .expect("handle_undelete_notifications");
    assert_eq!(undel_res, PyRep::Integer(2));
    let undel_n = drain_all(&ctx, &mut sess, PILOT_A);
    assert_eq!(
        find_notif(&undel_n, "OnNotificationUndeleted"),
        &[PyRep::List(vec![PyRep::Integer(501), PyRep::Integer(502)])]
    );

    notify_emitters_missing::emit_contextual_offer_received(&ctx, PILOT_A, 4401, 77_001, 110);
    let offer_n = drain_all(&ctx, &mut sess, PILOT_A);
    assert_eq!(find_notif(&offer_n, "OnOfferReceived")[0], PyRep::Integer(4401));

    notify_emitters_missing::emit_plex_deposit_failed(&ctx, PILOT_A, 9001, 500, "InsufficientPlex");
    let dep_n = drain_all(&ctx, &mut sess, PILOT_A);
    assert_eq!(
        find_notif(&dep_n, "OnPlexDepositFailed"),
        &[PyRep::Integer(9001), PyRep::Integer(500), PyRep::String("InsufficientPlex".into())]
    );
}

#[tokio::test]
async fn test_parity_notifies_missing_plex_dogma_and_ui() {
    // parity: notify:OnPlexWithdrawFailed, notify:OnProgressionTaskWidgetUpdate, notify:OnRecommendationsChanged
    // parity: notify:OnServerBrainUpdated, notify:OnServerMapDataPush, notify:OnStartConversation
    // parity: notify:OnUpdateLocalizationTextCache, notify:OnUpdateServiceMapping
    eve_testkit::attest(&[
        "notify:OnPlexWithdrawFailed",
        "notify:OnProgressionTaskWidgetUpdate",
        "notify:OnRecommendationsChanged",
        "notify:OnServerBrainUpdated",
        "notify:OnServerMapDataPush",
        "notify:OnStartConversation",
        "notify:OnUpdateLocalizationTextCache",
        "notify:OnUpdateServiceMapping",
    ]);
    let ctx = new_ctx();
    let mut sess = setup_pilot(&ctx, 1203).await;

    notify_emitters_missing::emit_plex_withdraw_failed(&ctx, PILOT_A, 9001, 250, "VaultLocked");
    let wd_n = drain_all(&ctx, &mut sess, PILOT_A);
    assert_eq!(
        find_notif(&wd_n, "OnPlexWithdrawFailed"),
        &[PyRep::Integer(9001), PyRep::Integer(250), PyRep::String("VaultLocked".into())]
    );

    notify_emitters_missing::emit_progression_task_widget_update(&ctx, PILOT_A, 10, 20, 3, 5);
    let prog_n = drain_all(&ctx, &mut sess, PILOT_A);
    assert_eq!(find_notif(&prog_n, "OnProgressionTaskWidgetUpdate")[0], PyRep::Integer(10));
    assert_eq!(find_notif(&prog_n, "OnProgressionTaskWidgetUpdate")[1], PyRep::Integer(20));

    notify_emitters_missing::emit_recommendations_changed(&ctx, PILOT_A);
    let rec_n = drain_all(&ctx, &mut sess, PILOT_A);
    assert_eq!(find_notif(&rec_n, "OnRecommendationsChanged").len(), 0);

    notify_emitters_missing::emit_server_brain_updated(&ctx, PILOT_A, 42, vec![1, 2, 3, 4]);
    let brain_n = drain_all(&ctx, &mut sess, PILOT_A);
    assert_eq!(
        find_notif(&brain_n, "OnServerBrainUpdated"),
        &[PyRep::Integer(42), PyRep::Buffer(vec![1, 2, 3, 4])]
    );

    notify_emitters_missing::emit_server_map_data_push(&ctx, PILOT_A, 30_000_142, 1200, 18);
    let map_n = drain_all(&ctx, &mut sess, PILOT_A);
    assert_eq!(
        find_notif(&map_n, "OnServerMapDataPush")[0].dict_get("solarSystemID").and_then(|v| v.as_i64()),
        Some(30_000_142)
    );

    notify_emitters_missing::emit_start_conversation(&ctx, PILOT_A, 601, 3_019_582, &[1, 2]);
    let conv_n = drain_all(&ctx, &mut sess, PILOT_A);
    assert_eq!(find_notif(&conv_n, "OnStartConversation")[0], PyRep::Integer(601));

    notify_emitters_missing::emit_update_localization_text_cache(&ctx, PILOT_A, &[(55_001, "en-us", "Updated")]);
    let loc_n = drain_all(&ctx, &mut sess, PILOT_A);
    assert_eq!(find_notif(&loc_n, "OnUpdateLocalizationTextCache").len(), 1);

    notify_emitters_missing::emit_update_service_mapping(&ctx, PILOT_A, &[("beyonce", 1001)]);
    let svc_n = drain_all(&ctx, &mut sess, PILOT_A);
    assert_eq!(find_notif(&svc_n, "OnUpdateServiceMapping").len(), 1);
}
