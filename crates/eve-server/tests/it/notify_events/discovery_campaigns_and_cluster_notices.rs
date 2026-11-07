#![forbid(unsafe_code)]
#[allow(unused_imports)]
use super::*;

pub(crate) mod parity_notify_batch {
    #[allow(unused_imports)]
    use super::*;

use crate::common;
use eve_db::service_roles;
use eve_machonet::SessionState;
use eve_proto::quasar::{EvePublicRequest, GoogleAny};
use eve_proto::PyRep;
use eve_server::quasar_gateway::STATUS_OK;
use eve_server::router::ServerContext;
use eve_server::sp6_lifecycle::tick_background_cluster;
use std::collections::HashMap;
use std::sync::Arc;

pub(crate) const PILOT_A: i64 = 90_000_001;
pub(crate) const PILOT_B: i64 = 90_000_002;
pub(crate) const JITA_SYSTEM: u32 = 30_000_142;

pub(crate) fn new_ctx() -> Arc<ServerContext> {
    let sde = Arc::new(
        eve_sde::SdeRegistry::open(&eve_sde::SdeRegistry::default_db_path())
            .expect("open evomu_sde.sqlite"),
    );
    let db = Arc::new(eve_db::WorldRepository::with_demo_seed());
    let ctx = Arc::new(ServerContext::with_sde_and_repository(sde, db));
    common::pin_clock(&ctx);
    ctx
}

pub(crate) async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, eve_proto::BridgeRpcError> {
    ctx.dispatch_rpc(sess, service, None, method, args, &HashMap::new(), None)
        .await
}

pub(crate) fn drain_events(
    ctx: &ServerContext,
    sess: &mut SessionState,
    char_id: i64,
) -> Vec<(String, Vec<PyRep>)> {
    let _ = sess.drain_notifications();
    ctx.drain_notifications_for_char(char_id)
        .into_iter()
        .map(|n| (n.event, n.args))
        .collect()
}

fn chat_bcast_req(msg: &str) -> EvePublicRequest {
    let mut payload = vec![0x0A, msg.len() as u8];
    payload.extend_from_slice(msg.as_bytes());
    EvePublicRequest {
        payload: Some(GoogleAny {
            type_url: "type.googleapis.com/eve_public.chat.local.api.BroadcastMessageRequest".into(),
            value: payload,
        }),
        ..Default::default()
    }
}

#[tokio::test]
async fn test_parity_notify_analysis_kredits_and_campaign_enrollment() {
    let ctx = new_ctx();
    let mut s1 = SessionState::new(92_001);
    common::login_pilot(&ctx, &mut s1, "demo", "demo-secret").await;
    s1.role = service_roles::ROLE_GM_DEFAULT;
    rpc(&ctx, &mut s1, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(PILOT_A)])
        .await
        .expect("select PILOT_A");
    s1.role = service_roles::ROLE_GM_DEFAULT;

    let mut s2 = SessionState::new(92_002);
    common::login_pilot(&ctx, &mut s2, "other", "other-secret").await;
    rpc(&ctx, &mut s2, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(PILOT_B)])
        .await
        .expect("select PILOT_B");

    let _ = drain_events(&ctx, &mut s1, PILOT_A);
    let _ = drain_events(&ctx, &mut s2, PILOT_B);

    // 1. OnAnalysisKreditsChange: negative check on invalid classification
    let err = rpc(
        &ctx,
        &mut s1,
        "ProjectDiscovery",
        "post_classification",
        &[PyRep::String("task_1".into()), PyRep::String("   ".into())],
    )
    .await
    .expect_err("empty classification must fail");
    assert_eq!(err.code, "InvalidClassification");
    assert!(
        !drain_events(&ctx, &mut s1, PILOT_A)
            .iter()
            .any(|(e, _)| e == "OnAnalysisKreditsChange"),
        "failed classification must not emit OnAnalysisKreditsChange"
    );

    // Positive: get_new_task -> post_classification with production data (no with_mut_snapshot)
    let ak_before = rpc(&ctx, &mut s1, "ProjectDiscovery", "get_player_analysis_kredits", &[])
        .await
        .expect("get_player_analysis_kredits before")
        .as_i64()
        .unwrap_or(-1);
    assert_eq!(ak_before, 0);

    let task = rpc(&ctx, &mut s1, "ProjectDiscovery", "get_new_task", &[])
        .await
        .expect("get_new_task");
    let task_id = task
        .dict_get("task_id")
        .and_then(|v| v.as_str())
        .expect("task_id")
        .to_string();

    let post_rep = rpc(
        &ctx,
        &mut s1,
        "ProjectDiscovery",
        "post_classification",
        &[
            PyRep::String(task_id),
            PyRep::List(vec![PyRep::Real(0.42), PyRep::Real(0.88)]),
        ],
    )
    .await
    .expect("post_classification");
    let ak_reward = post_rep
        .dict_get("AK_Reward")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    assert!(ak_reward > 0, "post_classification must grant AK_Reward > 0");

    let _ = s1.drain_notifications();
    let ak_notifs: Vec<Vec<PyRep>> = ctx
        .drain_notifications_for_char(PILOT_A)
        .into_iter()
        .filter(|n| n.event == "OnAnalysisKreditsChange")
        .map(|n| n.args)
        .collect();
    assert_eq!(ak_notifs.len(), 1, "must emit 1 OnAnalysisKreditsChange");
    assert_eq!(ak_notifs[0].len(), 0, "OnAnalysisKreditsChange expects 0 args");

    let ak_after = rpc(&ctx, &mut s1, "ProjectDiscovery", "get_player_analysis_kredits", &[])
        .await
        .expect("get_player_analysis_kredits after")
        .as_i64()
        .unwrap_or(0);
    assert_eq!(ak_after, ak_before + ak_reward);

    // 2. OnCampaignEnrollmentChanged: negative check on non-existent campaign
    let bad_camp = rpc(
        &ctx,
        &mut s1,
        "slash",
        "SlashCmd",
        &[PyRep::String("/campaign enroll 999999999".into())],
    )
    .await
    .expect_err("unknown campaign must fail");
    assert_eq!(bad_camp.code, "CampaignNotFound");

    // Positive: enroll PILOT_A in an alternate SDE campaign; verify PILOT_B is unaffected
    let campaigns = eve_server::services::campaigns_and_milestones::get_sde_login_campaigns(&ctx.sde);
    let alt_campaign = campaigns
        .keys()
        .copied()
        .find(|&id| id != 2261)
        .expect("at least 2 SDE login campaigns");
    let p2_before = rpc(&ctx, &mut s2, "loginCampaignManager", "get_client_campaign_state", &[])
        .await
        .expect("p2 campaign state before");

    rpc(
        &ctx,
        &mut s1,
        "slash",
        "SlashCmd",
        &[PyRep::String(format!("/campaign enroll {alt_campaign} {PILOT_A}"))],
    )
    .await
    .expect("/campaign enroll");

    let camp_notifs: Vec<Vec<PyRep>> = drain_events(&ctx, &mut s1, PILOT_A)
        .into_iter()
        .filter(|(e, _)| e == "OnCampaignEnrollmentChanged")
        .map(|(_, a)| a)
        .collect();
    assert_eq!(camp_notifs.len(), 1, "PILOT_A must receive OnCampaignEnrollmentChanged");
    assert_eq!(camp_notifs[0].len(), 0, "OnCampaignEnrollmentChanged expects 0 args");
    assert!(
        !drain_events(&ctx, &mut s2, PILOT_B)
            .iter()
            .any(|(e, _)| e == "OnCampaignEnrollmentChanged"),
        "PILOT_B must not receive OnCampaignEnrollmentChanged"
    );

    let p1_after = rpc(&ctx, &mut s1, "loginCampaignManager", "get_client_campaign_state", &[])
        .await
        .expect("p1 campaign state after");
    let p2_after = rpc(&ctx, &mut s2, "loginCampaignManager", "get_client_campaign_state", &[])
        .await
        .expect("p2 campaign state after");
    assert_eq!(p1_after.dict_get("campaignID"), Some(&PyRep::Integer(alt_campaign)));
    assert_eq!(p2_after.dict_get("campaignID"), p2_before.dict_get("campaignID"));
}

#[tokio::test]
async fn test_parity_notify_chat_bans_and_cluster_shutdown() {
    let ctx = new_ctx();
    let mut gm = SessionState::new(92_003);
    common::login_pilot(&ctx, &mut gm, "demo", "demo-secret").await;
    gm.role = service_roles::ROLE_GM_DEFAULT;
    rpc(&ctx, &mut gm, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(PILOT_A)])
        .await
        .expect("select PILOT_A");
    gm.role = service_roles::ROLE_GM_DEFAULT;

    let mut p2 = SessionState::new(92_004);
    common::login_pilot(&ctx, &mut p2, "other", "other-secret").await;
    rpc(&ctx, &mut p2, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(PILOT_B)])
        .await
        .expect("select PILOT_B");

    let _ = drain_events(&ctx, &mut gm, PILOT_A);
    let _ = drain_events(&ctx, &mut p2, PILOT_B);

    // 1. Permanent chat ban: blocks both Quasar BroadcastMessageRequest and MachoNet LSC.SendMessage
    rpc(
        &ctx,
        &mut gm,
        "slash",
        "SlashCmd",
        &[PyRep::String(format!("/chatban {PILOT_B} perm"))],
    )
    .await
    .expect("/chatban perm");

    let perm_notifs: Vec<Vec<PyRep>> = drain_events(&ctx, &mut p2, PILOT_B)
        .into_iter()
        .filter(|(e, _)| e == "OnCharacterPermanentlyBanned")
        .map(|(_, a)| a)
        .collect();
    assert_eq!(perm_notifs.len(), 1);
    assert_eq!(perm_notifs[0].len(), 0, "OnCharacterPermanentlyBanned expects 0 args");

    let quasar_blocked = ctx.quasar_gateway.handle_public_request(
        &p2,
        &chat_bcast_req("hello"),
        None,
        false,
        ctx.now_unix(),
    );
    assert_ne!(quasar_blocked.status_code, STATUS_OK, "Quasar chat must be blocked");
    let lsc_blocked = rpc(
        &ctx,
        &mut p2,
        "LSC",
        "SendMessage",
        &[PyRep::String("solarsystemid2:30000142".into()), PyRep::String("hello".into())],
    )
    .await
    .expect_err("LSC.SendMessage must be blocked");
    assert_eq!(lsc_blocked.code, "CharacterChatBanned");

    // 2. Unban: emits OnCharacterUnbanned() and restores chat
    rpc(
        &ctx,
        &mut gm,
        "slash",
        "SlashCmd",
        &[PyRep::String(format!("/chatunban {PILOT_B}"))],
    )
    .await
    .expect("/chatunban");

    let unban_notifs: Vec<Vec<PyRep>> = drain_events(&ctx, &mut p2, PILOT_B)
        .into_iter()
        .filter(|(e, _)| e == "OnCharacterUnbanned")
        .map(|(_, a)| a)
        .collect();
    assert_eq!(unban_notifs.len(), 1);
    assert_eq!(unban_notifs[0].len(), 0, "OnCharacterUnbanned expects 0 args");

    let quasar_ok = ctx.quasar_gateway.handle_public_request(
        &p2,
        &chat_bcast_req("unbanned"),
        None,
        false,
        ctx.now_unix(),
    );
    assert_eq!(quasar_ok.status_code, STATUS_OK, "Quasar chat must succeed after unban");

    // 3. Temporary chat ban: emits OnCharacterTemporarilyBanned(banExpiry), blocks chat, lapses after expiry
    let expected_expiry_ft = ctx.now_blueos_filetime_ticks() + 3600 * 10_000_000;
    rpc(
        &ctx,
        &mut gm,
        "slash",
        "SlashCmd",
        &[PyRep::String(format!("/chatban {PILOT_B} 3600"))],
    )
    .await
    .expect("/chatban 3600");

    let temp_notifs: Vec<Vec<PyRep>> = drain_events(&ctx, &mut p2, PILOT_B)
        .into_iter()
        .filter(|(e, _)| e == "OnCharacterTemporarilyBanned")
        .map(|(_, a)| a)
        .collect();
    assert_eq!(temp_notifs.len(), 1);
    assert_eq!(
        temp_notifs[0],
        vec![PyRep::Integer(expected_expiry_ft)],
        "OnCharacterTemporarilyBanned expects [banExpiry]"
    );

    let quasar_temp_blocked = ctx.quasar_gateway.handle_public_request(
        &p2,
        &chat_bcast_req("still muted"),
        None,
        false,
        ctx.now_unix(),
    );
    assert_ne!(quasar_temp_blocked.status_code, STATUS_OK);

    ctx.advance_secs(3601);
    let quasar_expired_ok = ctx.quasar_gateway.handle_public_request(
        &p2,
        &chat_bcast_req("expired"),
        None,
        false,
        ctx.now_unix(),
    );
    assert_eq!(quasar_expired_ok.status_code, STATUS_OK, "temp ban must lapse after expiry");

    // 4. OnClusterShutdownInitiated: scheduled daily downtime countdown via tick_background_cluster + GM /shutdown
    let _ = drain_events(&ctx, &mut p2, PILOT_B);
    let now = ctx.now_unix();
    let dt_sec = ctx.systems.downtime_utc_secs_of_day();
    let next_dt = eve_server::system_actor::next_downtime_unix_secs(now, 86_400, dt_sec);
    let jump_to_59m = next_dt.saturating_sub(59 * 60);
    if jump_to_59m > now {
        ctx.advance_secs(jump_to_59m - now);
    }
    tick_background_cluster(&ctx).await;

    let shut_notifs: Vec<Vec<PyRep>> = drain_events(&ctx, &mut p2, PILOT_B)
        .into_iter()
        .filter(|(e, _)| e == "OnClusterShutdownInitiated")
        .map(|(_, a)| a)
        .collect();
    assert_eq!(shut_notifs.len(), 1, "60m downtime warning must emit OnClusterShutdownInitiated");
    assert_eq!(shut_notifs[0].len(), 3, "OnClusterShutdownInitiated expects 3 args");
    assert_eq!(
        shut_notifs[0][0],
        PyRep::String("UI/Shared/ShutdownNotificationTitle".into())
    );
    let expected_dt_ft = (next_dt as i64 + 11_644_473_600) * 10_000_000;
    assert_eq!(shut_notifs[0][1], PyRep::Integer(expected_dt_ft));
    assert_eq!(shut_notifs[0][2], PyRep::Integer(15));
}


}
