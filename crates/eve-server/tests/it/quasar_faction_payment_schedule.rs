//! Integration tests for `eve_public.faction.activity.admin` and `eve_public.payment.schedule.api`
//! via live `publicGatewaySvc.SendRequest`, verifying decoded protobuf fields, persistence,
//! notice emission, and negative/auth cases.

#![forbid(unsafe_code)]

use crate::quasar_sov_equinox_hub::{drain, field, fields, id_req, idm, rpc, sess, subscribe, varint};
use eve_db::{FwSystemOccupationRecord, SystemDynamicState};
use eve_proto::quasar::{encode_bytes_field, encode_string_field, encode_varint_field};
use eve_server::quasar_gateway::{
    STATUS_BAD_REQUEST, STATUS_FORBIDDEN, STATUS_NOT_FOUND, STATUS_OK,
};
use eve_server::router::ServerContext;
use eve_server::services::quasar_payment_store_types::{
    save_store_catalog, PaymentScheduleRecord, PaymentTokenRecord, StoreCatalog, StoreCategory,
    StoreOffer,
};

const PILOT: i64 = 90_000_001;
const CORP: i64 = 98_000_001;
const USER_ID: u64 = 1;
const FW_SYS: u32 = 30002537; // Amamake (FW warzone system)
const NON_FW_SYS: u32 = 30000142; // Jita (non-FW system)
const ROLE_GMH: u64 = eve_machonet::service_roles::ROLE_GMH;

fn idm_val(bytes: &[u8], n: u32) -> Option<u64> {
    field(bytes, n).and_then(|b| varint(&b, 1))
}

fn encode_admin_score_req(sys_id: u32, faction_id: u32, score_or_delta: u64) -> Vec<u8> {
    let mut out = id_req(1, sys_id as u64);
    encode_bytes_field(2, &idm(faction_id as u64), &mut out);
    encode_varint_field(3, score_or_delta, &mut out);
    out
}

#[tokio::test]
async fn test_faction_activity_admin_increment_set_notices_and_auth() {
    // parity: quasar:eveProto/generated/eve_public/faction/activity/admin_pb2.pyj
    let ctx = ServerContext::new();
    ctx.db.with_mut_snapshot(|snap| {
        snap.fw_system_occupations.insert(
            FW_SYS,
            FwSystemOccupationRecord {
                solar_system_id: FW_SYS,
                owner_faction_id: 500001,
                occupier_faction_id: 500001,
                victory_points: 100,
                victory_points_threshold: 1000,
            },
        );
        snap.system_states.insert(
            FW_SYS,
            SystemDynamicState {
                solar_system_id: FW_SYS,
                fw_contested_pct: 10.0,
                ..Default::default()
            },
        );
        snap.fw_runtime.system_victory_points.insert(FW_SYS, 100);
        snap.fw_runtime.system_advantage.insert(FW_SYS, (0.25, 0.10));
        if let Some(c) = snap.characters.get_mut(&PILOT) {
            c.solar_system_id = FW_SYS;
        }
    });

    let mut gm_sess = sess(10, PILOT, CORP, FW_SYS, 0);
    gm_sess.role = ROLE_GMH;
    let mut rx = subscribe(&ctx, &mut gm_sess).await;

    // 1. IncrementContributionScoreRequest (+250 VP)
    let inc_req = encode_admin_score_req(FW_SYS, 500001, 250);
    let rsp = rpc(
        &ctx,
        &mut gm_sess,
        "eve_public.faction.activity.admin.IncrementContributionScoreRequest",
        inc_req,
    )
    .await;
    assert_eq!(rsp.status_code, STATUS_OK);

    // Verify persisted VP & contested percentage
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.fw_runtime.system_victory_points.get(&FW_SYS), Some(&350));
        let occ = snap.fw_system_occupations.get(&FW_SYS).unwrap();
        assert_eq!(occ.victory_points, 350);
        let st_rec = snap.system_states.get(&FW_SYS).unwrap();
        assert!((st_rec.fw_contested_pct - 35.0).abs() < 0.01);
    });

    // Verify ScoreContributionNotice & SolarSystemScoresNotice
    let notices = drain(&mut rx);
    let (_, contrib_val) = notices
        .iter()
        .find(|(ty, _)| ty == "eve_public.faction.activity.ScoreContributionNotice")
        .expect("ScoreContributionNotice");
    assert_eq!(idm_val(contrib_val, 1), Some(FW_SYS as u64));
    assert_eq!(idm_val(contrib_val, 2), Some(PILOT as u64));
    assert_eq!(idm_val(contrib_val, 3), Some(500001));
    assert_eq!(varint(contrib_val, 4), Some(250));

    let (_, scores_val) = notices
        .iter()
        .find(|(ty, _)| ty == "eve_public.faction.activity.SolarSystemScoresNotice")
        .expect("SolarSystemScoresNotice");
    assert_eq!(idm_val(scores_val, 1), Some(FW_SYS as u64));
    let entry = field(scores_val, 2).expect("score entry");
    assert_eq!(idm_val(&entry, 1), Some(500001));
    assert_eq!(varint(&entry, 2), Some(350));
    assert_eq!(varint(&entry, 3), Some(25));

    // 2. SetContributionScoreRequest (set to 600 VP, faction 500002)
    let set_req = encode_admin_score_req(FW_SYS, 500002, 600);
    let rsp_set = rpc(
        &ctx,
        &mut gm_sess,
        "eve_public.faction.activity.admin.SetContributionScoreRequest",
        set_req,
    )
    .await;
    assert_eq!(rsp_set.status_code, STATUS_OK);

    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.fw_runtime.system_victory_points.get(&FW_SYS), Some(&600));
        let occ = snap.fw_system_occupations.get(&FW_SYS).unwrap();
        assert_eq!(occ.victory_points, 600);
        assert_eq!(occ.occupier_faction_id, 500002);
    });

    let notices2 = drain(&mut rx);
    let (_, scores2_val) = notices2
        .iter()
        .find(|(ty, _)| ty == "eve_public.faction.activity.SolarSystemScoresNotice")
        .expect("SolarSystemScoresNotice after Set");
    let entry2 = field(scores2_val, 2).expect("score entry 2");
    assert_eq!(idm_val(&entry2, 1), Some(500002));
    assert_eq!(varint(&entry2, 2), Some(600));

    // 3. Negative / Auth cases
    // 3a. Non-FW solar system -> 400
    let rsp_bad = rpc(
        &ctx,
        &mut gm_sess,
        "eve_public.faction.activity.admin.IncrementContributionScoreRequest",
        encode_admin_score_req(NON_FW_SYS, 500001, 100),
    )
    .await;
    assert_eq!(rsp_bad.status_code, STATUS_BAD_REQUEST);

    // 3b. Non-GM session -> 403
    let mut non_gm = sess(11, PILOT, CORP, FW_SYS, 0);
    non_gm.role = 0;
    let rsp_forbid = rpc(
        &ctx,
        &mut non_gm,
        "eve_public.faction.activity.admin.IncrementContributionScoreRequest",
        encode_admin_score_req(FW_SYS, 500001, 100),
    )
    .await;
    assert_eq!(rsp_forbid.status_code, STATUS_FORBIDDEN);

    // 3c. Unauthenticated session (no character_id) -> 403
    let mut unauth = sess(12, PILOT, CORP, FW_SYS, 0);
    unauth.role = ROLE_GMH;
    unauth.character_id = None;
    let rsp_unauth = rpc(
        &ctx,
        &mut unauth,
        "eve_public.faction.activity.admin.IncrementContributionScoreRequest",
        encode_admin_score_req(FW_SYS, 500001, 100),
    )
    .await;
    assert_eq!(rsp_unauth.status_code, STATUS_FORBIDDEN);
}

#[tokio::test]
async fn test_payment_schedule_lifecycle_from_omega_purchase_and_auth() {
    // parity: quasar:eveProto/generated/eve_public/payment/schedule/requests_pb2.pyj
    let ctx = ServerContext::new();
    ctx.db.with_mut_snapshot(|snap| {
        save_store_catalog(
            snap,
            &StoreCatalog {
                categories: vec![StoreCategory {
                    id: 1,
                    name: "Omega".to_string(),
                    description: "Omega Clone State".to_string(),
                }],
                products: vec![],
                offers: vec![StoreOffer {
                    id: 1001,
                    name: "30 Days Omega".to_string(),
                    description: "30 Days Omega Subscription".to_string(),
                    category_id: 1,
                    price_plex: 500,
                    product_ids: vec![],
                    targeted: false,
                    price_cents: Some(1999),
                    currency: Some("USD".to_string()),
                }],
            },
        );
        let tokens = vec![PaymentTokenRecord {
            id: 77,
            user_id: USER_ID,
            token_str: "tok_live_omega_77".to_string(),
            is_quick_pay: true,
            consumed: false,
        }];
        snap.service_state_blobs.insert(
            "payment_tokens".to_string(),
            serde_json::to_string(&tokens).unwrap(),
        );
    });

    let mut player_sess = sess(20, PILOT, CORP, NON_FW_SYS, 0);

    // 1. Purchase Omega offer via TokenRequest -> creates PaymentScheduleRecord
    let mut purchase_req = Vec::new();
    let mut order = Vec::new();
    let mut off_ident = Vec::new();
    encode_string_field(1, "VEVC-1001", &mut off_ident);
    encode_bytes_field(1, &off_ident, &mut order);
    encode_varint_field(2, 1, &mut order);
    encode_bytes_field(1, &order, &mut purchase_req);
    encode_bytes_field(2, &idm(77), &mut purchase_req);

    let rsp_buy = rpc(
        &ctx,
        &mut player_sess,
        "eve_public.payment.purchase.api.TokenRequest",
        purchase_req,
    )
    .await;
    assert_eq!(rsp_buy.status_code, STATUS_OK);

    // Verify payment_schedules blob was populated by live purchase
    let persisted_scheds: Vec<PaymentScheduleRecord> = ctx.db.with_snapshot(|snap| {
        snap.service_state_blobs
            .get("payment_schedules")
            .and_then(|j| serde_json::from_str(j).ok())
            .unwrap_or_default()
    });
    assert_eq!(persisted_scheds.len(), 1);
    assert_eq!(persisted_scheds[0].user_id, USER_ID);
    assert_eq!(persisted_scheds[0].offer_id, "VEVC-1001");
    assert_eq!(persisted_scheds[0].interval_days, 30);

    // 2. GetActiveByTypeRequest (type = 1) -> returns ScheduleIdentifier
    let mut by_type_req = Vec::new();
    encode_varint_field(1, 1, &mut by_type_req);
    let rsp_type = rpc(
        &ctx,
        &mut player_sess,
        "eve_public.payment.schedule.api.GetActiveByTypeRequest",
        by_type_req,
    )
    .await;
    assert_eq!(rsp_type.status_code, STATUS_OK);
    let by_type_val = rsp_type.payload.unwrap().value;
    let sched_id = idm_val(&by_type_val, 1).expect("ScheduleIdentifier.id");
    assert_eq!(sched_id, persisted_scheds[0].id);

    // 3. GetRequest (id = sched_id) -> returns Attributes
    let rsp_get = rpc(
        &ctx,
        &mut player_sess,
        "eve_public.payment.schedule.api.GetRequest",
        id_req(1, sched_id),
    )
    .await;
    assert_eq!(rsp_get.status_code, STATUS_OK);
    let get_val = rsp_get.payload.unwrap().value;
    let attr = field(&get_val, 1).expect("Attributes");
    assert_eq!(varint(&attr, 1), Some(1)); // schedule_type
    let next_pay = field(&attr, 2).expect("next_payment");
    assert!(varint(&next_pay, 1).unwrap_or(0) > 0);
    let off_ident = field(&attr, 3).expect("offer");
    let off_str = fields(&off_ident, 1)
        .into_iter()
        .next()
        .and_then(|b| String::from_utf8(b).ok());
    assert_eq!(off_str.as_deref(), Some("VEVC-1001"));
    assert_eq!(varint(&attr, 4), Some(30)); // recurring_interval_days

    // 4. Negative cases
    let rsp_missing = rpc(
        &ctx,
        &mut player_sess,
        "eve_public.payment.schedule.api.GetRequest",
        id_req(1, 999_999),
    )
    .await;
    assert_eq!(rsp_missing.status_code, STATUS_NOT_FOUND);

    let mut bad_type_req = Vec::new();
    encode_varint_field(1, 99, &mut bad_type_req);
    let rsp_no_type = rpc(
        &ctx,
        &mut player_sess,
        "eve_public.payment.schedule.api.GetActiveByTypeRequest",
        bad_type_req,
    )
    .await;
    assert_eq!(rsp_no_type.status_code, STATUS_NOT_FOUND);

    // 5. Stacked Omega renewal extends `next_payment_unix` to cumulative `new_expiry`, and unauthenticated calls get 401
    let first_next = persisted_scheds[0].next_payment_unix;
    ctx.db.with_mut_snapshot(|snap| {
        let tokens = vec![PaymentTokenRecord {
            id: 78,
            user_id: USER_ID,
            token_str: "tok_live_omega_78".to_string(),
            is_quick_pay: true,
            consumed: false,
        }];
        snap.service_state_blobs.insert(
            "payment_tokens".to_string(),
            serde_json::to_string(&tokens).unwrap(),
        );
    });
    let mut renew_req = Vec::new();
    let mut order2 = Vec::new();
    let mut off_ident2 = Vec::new();
    encode_string_field(1, "VEVC-1001", &mut off_ident2);
    encode_bytes_field(1, &off_ident2, &mut order2);
    encode_varint_field(2, 1, &mut order2);
    encode_bytes_field(1, &order2, &mut renew_req);
    encode_bytes_field(2, &idm(78), &mut renew_req);
    assert_eq!(
        rpc(
            &ctx,
            &mut player_sess,
            "eve_public.payment.purchase.api.TokenRequest",
            renew_req,
        )
        .await
        .status_code,
        STATUS_OK
    );
    let stacked_scheds: Vec<PaymentScheduleRecord> = ctx.db.with_snapshot(|snap| {
        snap.service_state_blobs
            .get("payment_schedules")
            .and_then(|j| serde_json::from_str(j).ok())
            .unwrap_or_default()
    });
    assert_eq!(stacked_scheds[0].next_payment_unix, first_next + 30 * 86_400);

    let mut unauth_sess = eve_machonet::SessionState::new(29);
    for method in [
        "eve_public.payment.token.api.GetQuickPayRequest",
        "eve_public.payment.token.api.GetRequest",
        "eve_public.payment.token.api.DisableAllRequest",
        "eve_public.payment.schedule.api.GetRequest",
    ] {
        let rsp_unauth = rpc(&ctx, &mut unauth_sess, method, id_req(1, 78)).await;
        assert_eq!(
            rsp_unauth.status_code,
            eve_server::quasar_gateway::STATUS_UNAUTHORIZED,
            "{method} must reject unauthenticated session"
        );
    }
}
