#![forbid(unsafe_code)]

use crate::common;
use crate::quasar_sov_equinox_hub::{drain, field, id_req, idm, rpc, sess, structure, subscribe, varint};
use eve_db::corp_roles;
use eve_db::snapshot_ext::FwSystemOccupationRecord;
use eve_proto::quasar::{encode_bytes_field, encode_string_field, encode_varint_field};
use eve_server::quasar_gateway::{STATUS_BAD_REQUEST, STATUS_FORBIDDEN, STATUS_OK};
use eve_server::router::ServerContext;
use eve_server::services::sov_services::TYPE_SOVEREIGNTY_HUB;

const PILOT: i64 = 90_000_001;
const CORP: i64 = 98_000_001;
const JITA: u32 = 30_000_142;
const PERIMETER: u32 = 30_000_144;
const URLEN: u32 = 30_000_138;

#[tokio::test]
async fn test_sov_hub_and_star_resource_configurations() {
    // parity: quasar:eveProto/generated/eve_public/sovereignty/hub/api/requests_pb2.pyj
    // parity: quasar:eveProto/generated/eve_public/sovereignty/resource/star/api/requests_pb2.pyj
    eve_testkit::attest(&[
        "quasar:eveProto/generated/eve_public/sovereignty/hub/api/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/sovereignty/resource/star/api/requests_pb2.pyj",
    ]);

    let ctx = ServerContext::new();
    let mut s = sess(92_001, PILOT, CORP, JITA, corp_roles::ROLE_STATION_MANAGER);

    let hub_id = 1_020_000_051;
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(hub_id, structure(hub_id, CORP, JITA, TYPE_SOVEREIGNTY_HUB));
    });

    let res = rpc(&ctx, &mut s, "eve_public.sovereignty.hub.api.GetResourcesRequest", id_req(1, hub_id as u64)).await;
    assert_eq!(res.status_code, STATUS_OK);
    let payload = res.payload.expect("GetResourcesResponse payload").value;
    let power = field(&payload, 1).expect("power pair");
    let workforce = field(&payload, 2).expect("workforce pair");
    assert!(varint(&power, 1).unwrap_or(0) > 0, "Jita star power > 0");
    assert!(varint(&workforce, 1).unwrap_or(0) > 0, "Jita planet workforce > 0");

    let star_res = rpc(
        &ctx,
        &mut s,
        "eve_public.sovereignty.resource.star.api.GetAllConfigurationsRequest",
        Vec::new(),
    )
    .await;
    assert_eq!(star_res.status_code, STATUS_OK);
    let star_payload = star_res.payload.expect("GetAllConfigurationsResponse payload").value;
    let cfg = field(&star_payload, 1).expect("star configuration");
    let star_id = field(&cfg, 1).and_then(|b| varint(&b, 1)).expect("star id");
    let star_power = varint(&cfg, 2).expect("star power");
    assert!(
        ctx.sde
            .celestials_in_system(JITA)
            .iter()
            .any(|c| c.group_id == 6 && c.celestial_id as u64 == star_id),
        "star_id {star_id} must match SDE star in Jita"
    );
    assert!(star_power > 0);
}

#[tokio::test]
async fn test_sov_hub_workforce_transit_relay_network() {
    // parity: quasar:eveProto/generated/eve_public/sovereignty/hub/workforce/api/requests_pb2.pyj
    // parity: quasar:eveProto/generated/eve_public/sovereignty/hub/workforce/api/notices_pb2.pyj
    eve_testkit::attest(&[
        "quasar:eveProto/generated/eve_public/sovereignty/hub/workforce/api/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/sovereignty/hub/workforce/api/notices_pb2.pyj",
    ]);

    let ctx = ServerContext::new();
    let mut s = sess(92_002, PILOT, CORP, JITA, corp_roles::ROLE_STATION_MANAGER);
    let mut rx = subscribe(&ctx, &mut s).await;

    let (hub_a, hub_b, hub_c) = (1_020_000_061, 1_020_000_062, 1_020_000_063);
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(hub_a, structure(hub_a, CORP, JITA, TYPE_SOVEREIGNTY_HUB));
        snap.structures.insert(hub_b, structure(hub_b, CORP, PERIMETER, TYPE_SOVEREIGNTY_HUB));
        snap.structures.insert(hub_c, structure(hub_c, CORP, URLEN, TYPE_SOVEREIGNTY_HUB));
    });

    // Export 1000 from Jita to Perimeter, and Import in Urlen from Jita.
    let mut exp = Vec::new();
    encode_bytes_field(3, &idm(PERIMETER as u64), &mut exp);
    encode_varint_field(2, 1_000, &mut exp);
    let mut exp_cfg = Vec::new();
    encode_bytes_field(3, &exp, &mut exp_cfg);
    let mut cfg_req_a = id_req(1, hub_a as u64);
    encode_bytes_field(2, &exp_cfg, &mut cfg_req_a);
    assert_eq!(
        rpc(&ctx, &mut s, "eve_public.sovereignty.hub.workforce.api.ConfigureRequest", cfg_req_a)
            .await
            .status_code,
        STATUS_OK
    );

    let mut imp = Vec::new();
    encode_bytes_field(1, &id_req(1, JITA as u64), &mut imp);
    let mut imp_cfg = Vec::new();
    encode_bytes_field(2, &imp, &mut imp_cfg);
    let mut cfg_req_c = id_req(1, hub_c as u64);
    encode_bytes_field(2, &imp_cfg, &mut cfg_req_c);
    assert_eq!(
        rpc(&ctx, &mut s, "eve_public.sovereignty.hub.workforce.api.ConfigureRequest", cfg_req_c)
            .await
            .status_code,
        STATUS_OK
    );
    drain(&mut rx);

    // Configure Perimeter hub as Transit relay (field 4 = true).
    let mut transit_cfg = Vec::new();
    encode_varint_field(4, 1, &mut transit_cfg);
    let mut cfg_req_b = id_req(1, hub_b as u64);
    encode_bytes_field(2, &transit_cfg, &mut cfg_req_b);
    assert_eq!(
        rpc(&ctx, &mut s, "eve_public.sovereignty.hub.workforce.api.ConfigureRequest", cfg_req_b)
            .await
            .status_code,
        STATUS_OK
    );

    let notices = drain(&mut rx);
    assert!(
        notices.iter().any(|(t, _)| {
            t == "eve_public.sovereignty.resource.transfer.workforce.api.StateChangedNotice"
        }),
        "Transit configuration must emit StateChangedNotice"
    );

    // Verify Urlen hub now imports 1000 workforce via Perimeter Transit relay.
    let st_res = rpc(
        &ctx,
        &mut s,
        "eve_public.sovereignty.hub.workforce.api.GetStateRequest",
        id_req(1, hub_c as u64),
    )
    .await;
    assert_eq!(st_res.status_code, STATUS_OK);
    let st_val = st_res.payload.unwrap().value;
    let state_msg = field(&st_val, 1).unwrap();
    let imp_state = field(&state_msg, 2).unwrap();
    let src_entry = field(&imp_state, 1).unwrap();
    assert_eq!(varint(&src_entry, 2), Some(1_000));
}

#[tokio::test]
async fn test_skinr_sequencing_job_natural_completion_notice() {
    // parity: quasar:eveProto/generated/eve_public/cosmetic/ship/skin/thirdparty/sequencing/job/api/notices_pb2.pyj
    eve_testkit::attest(&[
        "quasar:eveProto/generated/eve_public/cosmetic/ship/skin/thirdparty/sequencing/job/api/notices_pb2.pyj",
    ]);

    let ctx = ServerContext::new();
    let mut gm = sess(92_003, PILOT, CORP, JITA, 0);
    gm.role = eve_machonet::service_roles::ROLE_GM_DEFAULT;
    let mut s = sess(92_004, PILOT, CORP, JITA, 0);
    let mut rx = subscribe(&ctx, &mut s).await;

    let skinr_cat = eve_server::services::quasar_cosmetics_skinr::SkinrStaticCatalog::load(&ctx.sde);
    let coating_comp = skinr_cat
        .components
        .values()
        .find(|c| !c.is_pattern() && !c.associated_types.is_empty())
        .expect("coating comp");
    let comp_id = coating_comp.component_id as u64;

    // Grant finite component license to PILOT
    let grant_req = id_req(1, comp_id);
    let mut grant_outer = Vec::new();
    encode_bytes_field(1, &grant_req, &mut grant_outer);
    encode_varint_field(2, 5, &mut grant_outer);
    assert_eq!(
        rpc(
            &ctx,
            &mut gm,
            "eve_public.cosmetic.ship.skin.thirdparty.component.license.finite.api.admin.GrantRequest",
            grant_outer,
        )
        .await
        .status_code,
        STATUS_OK
    );

    // Initiate sequencing job for Rifter (type 587)
    let mut slot_msg = Vec::new();
    encode_bytes_field(1, &idm(1), &mut slot_msg);
    let mut coat_cfg = Vec::new();
    encode_bytes_field(1, &idm(comp_id), &mut coat_cfg);
    let mut cfg_outer = Vec::new();
    encode_bytes_field(1, &coat_cfg, &mut cfg_outer);
    encode_bytes_field(2, &cfg_outer, &mut slot_msg);

    let mut layout = Vec::new();
    encode_bytes_field(1, &slot_msg, &mut layout);
    encode_bytes_field(2, &idm(587), &mut layout);
    encode_varint_field(3, 1, &mut layout);

    let mut init_req = Vec::new();
    encode_string_field(1, "T1T3T5 Test Skin", &mut init_req);
    encode_string_field(2, "Test Line", &mut init_req);
    encode_bytes_field(3, &layout, &mut init_req);
    encode_varint_field(5, 1, &mut init_req);

    let init_res = rpc(
        &ctx,
        &mut s,
        "eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.api.InitiateRequest",
        init_req,
    )
    .await;
    assert_eq!(init_res.status_code, STATUS_OK);
    drain(&mut rx);

    // Advance clock by 30 days so planned_duration_secs elapses naturally
    ctx.clock.read().unwrap().advance_secs(30 * 86_400);

    let active_res = rpc(
        &ctx,
        &mut s,
        "eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.api.GetAllActiveRequest",
        Vec::new(),
    )
    .await;
    assert_eq!(active_res.status_code, STATUS_OK);
    let notices = drain(&mut rx);
    assert!(
        notices.iter().any(|(t, _)| {
            t == "eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.api.CompletedNotice"
        }),
        "Natural completion must emit CompletedNotice"
    );
}

#[tokio::test]
async fn test_payment_schedule_omega_purchase_and_faction_activity_admin() {
    // parity: quasar:eveProto/generated/eve_public/payment/schedule/requests_pb2.pyj
    // parity: quasar:eveProto/generated/eve_public/faction/activity/admin_pb2.pyj
    eve_testkit::attest(&[
        "quasar:eveProto/generated/eve_public/payment/schedule/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/faction/activity/admin_pb2.pyj",
    ]);

    let ctx = ServerContext::new();
    common::seed_test_nes_store_catalog(&ctx);
    ctx.db.with_mut_snapshot(|snap| {
        let tokens = vec![eve_server::services::quasar_payment_store::PaymentTokenRecord {
            id: 1,
            user_id: 1,
            token_str: "tok_visa_4242".into(),
            is_quick_pay: true,
            consumed: false,
        }];
        snap.service_state_blobs
            .insert("payment_tokens".to_string(), serde_json::to_string(&tokens).unwrap());
    });
    let mut s = sess(92_005, PILOT, CORP, JITA, 0);

    // Purchase Omega offer "1001" via TokenRequest -> creates payment schedule
    let mut off_ident = Vec::new();
    encode_string_field(1, "1001", &mut off_ident);
    let mut order_msg = Vec::new();
    encode_bytes_field(1, &off_ident, &mut order_msg);
    let mut purchase_req = Vec::new();
    encode_bytes_field(1, &order_msg, &mut purchase_req);
    encode_bytes_field(2, &idm(1), &mut purchase_req);
    let p_res = rpc(&ctx, &mut s, "eve_public.payment.purchase.api.TokenRequest", purchase_req).await;
    assert_eq!(p_res.status_code, STATUS_OK);

    let mut by_type_req = Vec::new();
    encode_varint_field(1, 1, &mut by_type_req);
    let active_sched = rpc(
        &ctx,
        &mut s,
        "eve_public.payment.schedule.api.GetActiveByTypeRequest",
        by_type_req,
    )
    .await;
    assert_eq!(active_sched.status_code, STATUS_OK);
    let active_payload = active_sched.payload.unwrap().value;
    let first_id = field(&active_payload, 1)
        .and_then(|b| varint(&b, 1))
        .expect("schedule id");

    let get_sched = rpc(
        &ctx,
        &mut s,
        "eve_public.payment.schedule.api.GetRequest",
        id_req(1, first_id),
    )
    .await;
    assert_eq!(get_sched.status_code, STATUS_OK);
    let sched_payload = get_sched.payload.unwrap().value;
    let sched_attr = field(&sched_payload, 1).expect("schedule attributes");
    assert_eq!(varint(&sched_attr, 1), Some(1), "Omega schedule type = 1");
    assert_eq!(varint(&sched_attr, 4), Some(30), "30-day recurring interval");

    // Faction activity admin: non-admin rejected, admin increments FW victory points
    ctx.db.with_mut_snapshot(|snap| {
        snap.fw_system_occupations.insert(
            30_002_537,
            FwSystemOccupationRecord {
                solar_system_id: 30_002_537,
                owner_faction_id: 500_001,
                occupier_faction_id: 500_001,
                victory_points: 100,
                victory_points_threshold: 3_000,
            },
        );
    });

    let mut inc_req = id_req(1, 30_002_537);
    encode_bytes_field(2, &idm(500_002), &mut inc_req);
    encode_varint_field(3, 250, &mut inc_req);

    assert_eq!(
        rpc(
            &ctx,
            &mut s,
            "eve_public.faction.activity.admin.IncrementContributionScoreRequest",
            inc_req.clone(),
        )
        .await
        .status_code,
        STATUS_FORBIDDEN
    );

    let mut gm = sess(92_006, PILOT, CORP, 30_002_537, 0);
    gm.role = eve_machonet::service_roles::ROLE_GM_DEFAULT;
    assert_eq!(
        rpc(
            &ctx,
            &mut gm,
            "eve_public.faction.activity.admin.IncrementContributionScoreRequest",
            inc_req,
        )
        .await
        .status_code,
        STATUS_OK
    );
    let vp = ctx
        .db
        .with_snapshot(|snap| snap.fw_runtime.system_victory_points.get(&30_002_537).copied())
        .unwrap_or(0);
    assert!(vp >= 250, "FW system victory points must reflect admin increment, got {vp}");
}

#[tokio::test]
async fn test_ai_assistant_chat_sde_query() {
    // parity: quasar:eveProto/generated/eve_public/ai/assistant/chat/api/requests_pb2.pyj
    eve_testkit::attest(&[
        "quasar:eveProto/generated/eve_public/ai/assistant/chat/api/requests_pb2.pyj",
    ]);

    let ctx = ServerContext::new();
    let mut s = sess(92_007, PILOT, CORP, JITA, 0);

    // Empty query rejected with STATUS_BAD_REQUEST
    let mut empty_req = Vec::new();
    encode_string_field(1, "   ", &mut empty_req);
    assert_eq!(
        rpc(&ctx, &mut s, "eve_public.ai.assistant.chat.api.QueryRequest", empty_req)
            .await
            .status_code,
        STATUS_BAD_REQUEST
    );

    // SDE solar system query ("Jita") -> fallback = false, contains Jita system ID
    let mut jita_req = Vec::new();
    encode_string_field(1, "Jita", &mut jita_req);
    let jita_res = rpc(&ctx, &mut s, "eve_public.ai.assistant.chat.api.QueryRequest", jita_req).await;
    assert_eq!(jita_res.status_code, STATUS_OK);
    let jita_val = jita_res.payload.expect("QueryResponse payload").value;
    let jita_text = String::from_utf8(field(&jita_val, 1).expect("response text")).expect("utf8");
    assert!(jita_text.contains("30000142"), "Expected Jita ID in response: {jita_text}");
    assert_eq!(varint(&jita_val, 2), Some(0), "SDE match must have fallback = false");

    // SDE item type query ("Rifter") -> fallback = false, contains Rifter type ID 587
    let mut rifter_req = Vec::new();
    encode_string_field(1, "Rifter", &mut rifter_req);
    let rifter_res = rpc(&ctx, &mut s, "eve_public.ai.assistant.chat.api.QueryRequest", rifter_req).await;
    assert_eq!(rifter_res.status_code, STATUS_OK);
    let rifter_val = rifter_res.payload.expect("QueryResponse payload").value;
    let rifter_text = String::from_utf8(field(&rifter_val, 1).expect("response text")).expect("utf8");
    assert!(rifter_text.contains("587"), "Expected Rifter type ID 587 in response: {rifter_text}");
    assert_eq!(varint(&rifter_val, 2), Some(0), "SDE match must have fallback = false");

    // Unmatched query -> fallback = true
    let mut unknown_req = Vec::new();
    encode_string_field(1, "ZzzNonexistentEntity999", &mut unknown_req);
    let unknown_res = rpc(&ctx, &mut s, "eve_public.ai.assistant.chat.api.QueryRequest", unknown_req).await;
    assert_eq!(unknown_res.status_code, STATUS_OK);
    let unknown_val = unknown_res.payload.expect("QueryResponse payload").value;
    assert_eq!(varint(&unknown_val, 2), Some(1), "Unmatched query must have fallback = true");
}

