//! Integration tests for Quasar PLEX Vault, NES Store, Payment Schedules/Tokens,
//! User Licenses, and AIR Achievements systems.

use crate::common;
use eve_machonet::SessionState;
use eve_proto::quasar::{
    encode_bytes_field, encode_string_field, encode_varint_field, EvePublicNotice,
    EvePublicRequest, EvePublicResponse, GoogleAny, GoogleTimestamp, QuasarMessage,
    TYPE_GOOGLEAPIS_PREFIX,
};
use eve_proto::PyRep;
use eve_server::quasar_gateway::{
    STATUS_BAD_REQUEST, STATUS_FORBIDDEN, STATUS_NOT_FOUND, STATUS_OK,
};
use eve_server::router::ServerContext;
use eve_server::services::quasar_auth_user::{activate_user_omega_license, expire_user_omega_license};
use eve_server::services::quasar_payment_store::{
    add_user_quick_pay_token, cancel_user_payment_schedule, PaymentScheduleRecord, PaymentTokenRecord,
};
use std::collections::HashMap;
use tokio::sync::mpsc::Receiver;

fn extract_response_proto(rep: &PyRep) -> EvePublicResponse {
    if let PyRep::KeyVal(entries) = rep {
        for (k, v) in entries {
            if k == "response_proto" {
                if let PyRep::Bytes(b) = v {
                    return EvePublicResponse::decode_proto(b).expect("decode proto");
                }
            }
        }
    }
    panic!("Expected response_proto, got {rep:?}");
}

fn build_sess(sid: u64, uid: u64, cid: u32) -> SessionState {
    let mut sess = SessionState::new(sid);
    sess.user_id = Some(uid);
    sess.character_id = Some(cid as i64);
    sess.corporation_id = Some(98_000_001);
    sess.solar_system_id = Some(30_000_142);
    sess.role = eve_machonet::service_roles::ROLE_GM_DEFAULT;
    sess
}

async fn quasar_rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    type_url: &str,
    payload_value: Vec<u8>,
) -> EvePublicResponse {
    let empty_kw = HashMap::new();
    let req = EvePublicRequest {
        issued: Some(GoogleTimestamp { seconds: ctx.now_unix() as i64, nanos: 0 }),
        correlation_uuid: b"vault-test-00001".to_vec(),
        payload: Some(GoogleAny {
            type_url: if type_url.starts_with(TYPE_GOOGLEAPIS_PREFIX) {
                type_url.to_string()
            } else {
                format!("{TYPE_GOOGLEAPIS_PREFIX}{type_url}")
            },
            value: payload_value,
        }),
        external_origin: "eve-client".into(),
        application_instance_uuid: b"app-vault-inst-1".to_vec(),
        authoritative_context: None,
        trace_context: None,
    };
    let rep = ctx
        .dispatch_rpc(sess, "publicGatewaySvc", None, "SendRequest", &[PyRep::Bytes(req.encode_proto())], &empty_kw, None)
        .await
        .expect("SendRequest");
    extract_response_proto(&rep)
}

fn drain_notices(rx: &mut Receiver<EvePublicNotice>) -> Vec<EvePublicNotice> {
    let mut out = Vec::new();
    for _ in 0..1024 {
        match rx.try_recv() {
            Ok(msg) => out.push(msg),
            Err(_) => break,
        }
    }
    out
}

fn flush_pending_notices(ctx: &ServerContext) {
    let queued: Vec<_> = ctx.quasar_gateway.pending_notices.write().unwrap_or_else(|e| e.into_inner()).drain(..).collect();
    for (scope, payload) in queued {
        let _ = ctx.quasar_gateway.publish_notice(scope, payload, ctx.now_unix());
    }
}

fn make_id_request(id: u32) -> Vec<u8> {
    let (mut uuid, mut b, mut out) = ([0u8; 16], Vec::new(), Vec::new());
    uuid[12..16].copy_from_slice(&id.to_be_bytes());
    encode_bytes_field(1, &uuid, &mut b);
    encode_bytes_field(1, &b, &mut out);
    out
}

fn make_admin_progress(id: u32, delta: u64) -> Vec<u8> {
    let mut out = make_id_request(id);
    encode_varint_field(2, delta, &mut out);
    out
}

fn make_varint_id_request(id: u64) -> Vec<u8> {
    let (mut b, mut out) = (Vec::new(), Vec::new());
    encode_varint_field(1, id, &mut b);
    encode_bytes_field(1, &b, &mut out);
    out
}

fn make_str_id_request(s: &str) -> Vec<u8> {
    let (mut b, mut out) = (Vec::new(), Vec::new());
    encode_string_field(1, s, &mut b);
    encode_bytes_field(1, &b, &mut out);
    out
}

fn make_ta_offer_request(offer_id: u32) -> Vec<u8> {
    let (mut entry, mut id_msg, mut out) = (Vec::new(), Vec::new(), Vec::new());
    encode_string_field(1, &offer_id.to_string(), &mut entry);
    encode_bytes_field(2, &entry, &mut id_msg);
    encode_bytes_field(1, &id_msg, &mut out);
    out
}

#[tokio::test]
async fn test_unified_plex_vault_and_transactions() {
    // parity: feat:wallet.plex-vault, quasar:eveProto/generated/eve_public/plex/vault/api/requests_pb2.pyj, quasar:eveProto/generated/eve_public/plex/vault/api/notices_pb2.pyj, quasar:eveProto/generated/eve_public/plex/vault/transaction/api/requests_pb2.pyj
    eve_testkit::attest(&[
        "feat:wallet.plex-vault",
        "quasar:eveProto/generated/eve_public/plex/vault/api/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/plex/vault/api/notices_pb2.pyj",
        "quasar:eveProto/generated/eve_public/plex/vault/transaction/api/requests_pb2.pyj",
    ]);
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let (uid, cid) = (1u64, 90_000_001);
    let mut sess = build_sess(3501, uid, cid);
    let mut notice_rx = ctx.quasar_gateway.subscribe_session(&sess, vec![], ctx.now_unix());

    // 1. Initial BalanceRequest -> matches canonical seeded vault balance (500)
    let rsp = quasar_rpc(&ctx, &mut sess, "eve_public.plex.vault.api.BalanceRequest", vec![]).await;
    assert_eq!(rsp.status_code, STATUS_OK, "BalanceRequest failed: {}", rsp.status_message);
    let init_bal = ctx.db.with_snapshot(|snap| snap.account_plex_vault.get(&uid).copied().unwrap_or(0));
    assert_eq!(init_bal, 500, "Initial PLEX balance for demo user 1 should be 500");

    // 2. GM Deposit 500 PLEX
    let (mut curr_msg, mut dep_payload) = (Vec::new(), Vec::new());
    encode_varint_field(1, eve_proto::quasar::encode_zigzag64(500 * 100), &mut curr_msg);
    encode_bytes_field(3, &curr_msg, &mut dep_payload);
    let dep_rsp = quasar_rpc(&ctx, &mut sess, "eve.plex.vault.assetholding.operation.api.DepositRequest", dep_payload).await;
    assert_eq!(dep_rsp.status_code, STATUS_OK, "DepositRequest failed: {}", dep_rsp.status_message);

    // Verify unified state across snapshot and MachoNet helper
    let (snap_bal, char_bal) = ctx.db.with_snapshot(|snap| (snap.account_plex_vault.get(&uid).copied().unwrap_or(0), snap.plex_vault_balance(cid as i64)));
    assert_eq!((snap_bal, char_bal), (1000, 1000));
    flush_pending_notices(&ctx);
    let notices = drain_notices(&mut notice_rx);
    assert!(notices.iter().any(|n| n.payload.as_ref().map(|p| p.type_url.contains("BalanceChangedNotice")).unwrap_or(false)));

    // 3. BalanceRequest -> reflects 1000 PLEX; GM Withdraw 200 PLEX -> 800 PLEX
    let rsp2 = quasar_rpc(&ctx, &mut sess, "eve_public.plex.vault.api.BalanceRequest", vec![]).await;
    assert_eq!(rsp2.payload.unwrap().normalized_type_name(), "eve_public.plex.vault.api.BalanceResponse");
    let (mut with_curr, mut with_payload) = (Vec::new(), Vec::new());
    encode_varint_field(1, eve_proto::quasar::encode_zigzag64(200 * 100), &mut with_curr);
    encode_bytes_field(3, &with_curr, &mut with_payload);
    assert_eq!(quasar_rpc(&ctx, &mut sess, "eve.plex.vault.assetholding.operation.api.WithdrawRequest", with_payload).await.status_code, STATUS_OK);
    assert_eq!(ctx.db.with_snapshot(|snap| snap.plex_vault_balance(cid as i64)), 800);

    // 4. NES Purchase via eve_public.plex.vault.api.PurchaseRequest: seed catalog, buy offer 2001 (450 PLEX -> 350 PLEX)
    common::seed_test_nes_store_catalog(&ctx);
    assert_eq!(quasar_rpc(&ctx, &mut sess, "eve_public.plex.vault.api.PurchaseRequest", make_str_id_request("9999")).await.status_code, STATUS_NOT_FOUND);
    let mut over_purch = make_str_id_request("1001");
    encode_varint_field(2, 10, &mut over_purch); // 10 * 500 = 5000 PLEX > 800
    assert_eq!(quasar_rpc(&ctx, &mut sess, "eve_public.plex.vault.api.PurchaseRequest", over_purch).await.status_code, STATUS_BAD_REQUEST);
    let (mut bad_gift, mut gm, mut cm) = (make_str_id_request("2001"), Vec::new(), Vec::new());
    encode_varint_field(1, 999_999_999, &mut cm);
    encode_bytes_field(3, &cm, &mut gm);
    encode_bytes_field(3, &gm, &mut bad_gift);
    assert_eq!(quasar_rpc(&ctx, &mut sess, "eve_public.plex.vault.api.PurchaseRequest", bad_gift).await.status_code, STATUS_NOT_FOUND);
    assert_eq!(ctx.db.with_snapshot(|snap| snap.plex_vault_balance(cid as i64)), 800, "Missing gift recipient must not debit PLEX");
    assert_eq!(quasar_rpc(&ctx, &mut sess, "eve_public.plex.vault.api.PurchaseRequest", make_str_id_request("2001")).await.status_code, STATUS_OK);
    assert_eq!(ctx.db.with_snapshot(|snap| snap.plex_vault_balance(cid as i64)), 350, "PurchaseRequest deducts 450 PLEX");

    // 5. GetAllLoggedForUserRequest & GetLogRequest & operation.api.GetRequest & RecordInvoiceRequest
    let tx_all_rsp = quasar_rpc(&ctx, &mut sess, "eve_public.plex.vault.transaction.api.GetAllLoggedForUserRequest", vec![]).await;
    assert_eq!(tx_all_rsp.status_code, STATUS_OK);
    let first_tx_id = ctx.db.with_snapshot(|snap| {
        eve_server::services::quasar_payment_store::get_plex_transactions(snap).into_iter().find(|t| t.user_id == uid).map(|t| t.id).expect("tx exists")
    });
    let log_rsp = quasar_rpc(&ctx, &mut sess, "eve_public.plex.vault.transaction.api.GetLogRequest", make_varint_id_request(first_tx_id)).await;
    assert_eq!(log_rsp.status_code, STATUS_OK);
    assert!(!log_rsp.payload.unwrap().value.is_empty());
    let op_rsp = quasar_rpc(&ctx, &mut sess, "eve.plex.vault.assetholding.operation.api.GetRequest", make_str_id_request(&first_tx_id.to_string())).await;
    assert_eq!(op_rsp.status_code, STATUS_OK);
    assert_eq!(quasar_rpc(&ctx, &mut sess, "eve.plex.vault.transaction.invoice.api.admin.RecordInvoiceRequest", make_str_id_request("inv-001")).await.status_code, STATUS_OK);
    assert_eq!(quasar_rpc(&ctx, &mut sess, "eve_public.plex.vault.transaction.api.GetLogRequest", make_varint_id_request(999_999_999)).await.status_code, STATUS_NOT_FOUND);
    let mut sess_user2 = build_sess(3509, 2, 90_000_002);
    sess_user2.role = 0;
    assert_eq!(quasar_rpc(&ctx, &mut sess_user2, "eve_public.plex.vault.transaction.api.GetLogRequest", make_varint_id_request(first_tx_id)).await.status_code, STATUS_NOT_FOUND);
    assert_eq!(quasar_rpc(&ctx, &mut sess_user2, "eve.plex.vault.assetholding.operation.api.GetRequest", make_str_id_request(&first_tx_id.to_string())).await.status_code, STATUS_FORBIDDEN);

    // Test MachoNet RPCs DepositPlexToVault and WithdrawPlexFromVault emitting BalanceChangedNotice (R-PS-01)
    let initial_vault = ctx.db.with_snapshot(|s| s.plex_vault_balance(cid as i64));
    let plex_item_id = ctx.db.add_or_stack_item(cid as i64, 60003760, eve_db::InventoryFlag::Hangar, 44992, 50).expect("add plex item").item_id;
    sess.station_id = Some(60003760);
    let dep_py = ctx.dispatch_rpc(
        &mut sess, "invbroker", None, "DepositPlexToVault",
        &[PyRep::Integer(60003760), PyRep::Integer(plex_item_id), PyRep::Integer(50)],
        &HashMap::new(), None,
    ).await.expect("DepositPlexToVault");
    assert!(matches!(dep_py, PyRep::Integer(_)));
    assert_eq!(ctx.db.with_snapshot(|s| s.plex_vault_balance(cid as i64)), initial_vault + 50);

    let with_py = ctx.dispatch_rpc(
        &mut sess, "invbroker", None, "WithdrawPlexFromVault",
        &[PyRep::Integer(60003760), PyRep::Integer(20)],
        &HashMap::new(), None,
    ).await.expect("WithdrawPlexFromVault");
    assert!(matches!(with_py, PyRep::KeyVal(_) | PyRep::Dict(_)));
    assert_eq!(ctx.db.with_snapshot(|s| s.plex_vault_balance(cid as i64)), initial_vault + 30);

    flush_pending_notices(&ctx);
    let m_notices = drain_notices(&mut notice_rx);
    let has_bal_notice = m_notices.iter().any(|n| n.payload.as_ref().map(|p| p.type_url.contains("BalanceChangedNotice")).unwrap_or(false));
    assert!(has_bal_notice, "BalanceChangedNotice must be emitted on DepositPlexToVault/WithdrawPlexFromVault");

    // 7. GetStatisticsRequest -> aggregated inflow/outflow
    let stats_rsp = quasar_rpc(&ctx, &mut sess, "eve_public.plex.vault.transaction.api.GetStatisticsRequest", vec![]).await;
    assert_eq!(stats_rsp.status_code, STATUS_OK);
    assert_eq!(stats_rsp.payload.unwrap().normalized_type_name(), "eve_public.plex.vault.transaction.api.GetStatisticsResponse");
}

#[tokio::test]
async fn test_quasar_store_catalog_products_and_offers() {
    // DATA GAP: requires SDE/live store catalog import (feat:wallet.nes-store, store/{category,offer,product,targetaudience}/api_pb2.pyj)
    let ctx = ServerContext::new();
    let mut sess = build_sess(3502, 1, 90_000_001);

    // 0. Unseeded catalog returns empty list and 404 Not Found
    let empty_cats = quasar_rpc(&ctx, &mut sess, "eve_public.store.category.api.GetAllRequest", vec![]).await;
    assert_eq!(empty_cats.status_code, STATUS_OK);
    assert!(empty_cats.payload.as_ref().unwrap().value.is_empty());
    assert_eq!(quasar_rpc(&ctx, &mut sess, "eve_public.store.offer.api.GetRequest", make_str_id_request("1001")).await.status_code, STATUS_NOT_FOUND);
    common::pin_clock(&ctx);
    common::seed_test_nes_store_catalog(&ctx);

    // 1. GetAllRequest (categories) & 2. GetRequest (category 1)
    let cat_all = quasar_rpc(&ctx, &mut sess, "eve_public.store.category.api.GetAllRequest", vec![]).await;
    assert_eq!(cat_all.status_code, STATUS_OK);
    assert_eq!(cat_all.payload.as_ref().unwrap().normalized_type_name(), "eve_public.store.category.api.GetAllResponse");
    let cat_get = quasar_rpc(&ctx, &mut sess, "eve_public.store.category.api.GetRequest", make_varint_id_request(1)).await;
    assert_eq!(cat_get.status_code, STATUS_OK, "GetRequest category 1 failed: {}", cat_get.status_message);
    assert_eq!(cat_get.payload.as_ref().unwrap().normalized_type_name(), "eve_public.store.category.api.GetResponse");
    let cat_miss = quasar_rpc(&ctx, &mut sess, "eve_public.store.category.api.GetRequest", make_varint_id_request(9999)).await;
    assert_eq!(cat_miss.status_code, STATUS_NOT_FOUND);

    // 3. GetRequest (product 101 - 30 Days Omega Clone State)
    let prod_rsp = quasar_rpc(&ctx, &mut sess, "eve_public.store.product.api.GetRequest", make_varint_id_request(101)).await;
    assert_eq!(prod_rsp.status_code, STATUS_OK, "product 101 failed: {}", prod_rsp.status_message);
    assert_eq!(prod_rsp.payload.as_ref().unwrap().normalized_type_name(), "eve_public.store.product.api.GetResponse");

    // Product 102 (Apocalypse Kador SKIN - category 91) and Product 104 (Men's Precision Boots - category 30)
    let prod_102 = quasar_rpc(&ctx, &mut sess, "eve_public.store.product.api.GetRequest", make_varint_id_request(102)).await;
    assert_eq!(prod_102.status_code, STATUS_OK);
    let prod_104 = quasar_rpc(&ctx, &mut sess, "eve_public.store.product.api.GetRequest", make_varint_id_request(104)).await;
    assert_eq!(prod_104.status_code, STATUS_OK);

    let cat_data = ctx.db.with_snapshot(eve_server::services::quasar_payment_store::read_store_catalog);
    let p102 = cat_data.products.iter().find(|p| p.id == 102).expect("p102");
    assert_eq!(p102.type_id, 34599);
    assert_eq!(ctx.sde.types.get(&p102.type_id).expect("sde 34599").category_id, 91);
    let p103 = cat_data.products.iter().find(|p| p.id == 103).expect("p103");
    assert_eq!(p103.type_id, 40519);
    assert_eq!(ctx.sde.types.get(&p103.type_id).expect("sde 40519").category_id, 5);
    let p104 = cat_data.products.iter().find(|p| p.id == 104).expect("p104");
    assert_eq!(p104.type_id, 3966);
    assert_eq!(ctx.sde.types.get(&p104.type_id).expect("sde 3966").category_id, 30);

    let prod_miss_rsp = quasar_rpc(&ctx, &mut sess, "eve_public.store.product.api.GetRequest", make_varint_id_request(9999)).await;
    assert_eq!(prod_miss_rsp.status_code, STATUS_NOT_FOUND);

    // 4. GetAllRequest (offers)
    let off_all = quasar_rpc(&ctx, &mut sess, "eve_public.store.offer.api.GetAllRequest", vec![]).await;
    assert_eq!(off_all.status_code, STATUS_OK);
    assert_eq!(off_all.payload.as_ref().unwrap().normalized_type_name(), "eve_public.store.offer.api.GetAllResponse");

    // 5. GetRequest (offer 1001)
    let off_rsp = quasar_rpc(&ctx, &mut sess, "eve_public.store.offer.api.GetRequest", make_str_id_request("1001")).await;
    assert_eq!(off_rsp.status_code, STATUS_OK, "offer 1001 failed: {}", off_rsp.status_message);
    assert_eq!(off_rsp.payload.as_ref().unwrap().normalized_type_name(), "eve_public.store.offer.api.GetResponse");

    let off_miss_rsp = quasar_rpc(&ctx, &mut sess, "eve_public.store.offer.api.GetRequest", make_str_id_request("9999")).await;
    assert_eq!(off_miss_rsp.status_code, STATUS_NOT_FOUND);

    // 6. GetAllTargetedRequest
    let off_tgt = quasar_rpc(&ctx, &mut sess, "eve_public.store.offer.api.GetAllTargetedRequest", vec![]).await;
    assert_eq!(off_tgt.status_code, STATUS_OK);
    assert_eq!(off_tgt.payload.as_ref().unwrap().normalized_type_name(), "eve_public.store.offer.api.GetAllTargetedResponse");

    // 7. targetaudience.offer.api.ListRequest
    let ta_list = quasar_rpc(&ctx, &mut sess, "eve_public.store.targetaudience.offer.api.ListRequest", vec![]).await;
    assert_eq!(ta_list.status_code, STATUS_OK);
    assert_eq!(ta_list.payload.as_ref().unwrap().normalized_type_name(), "eve_public.store.targetaudience.offer.api.ListResponse");

    // 8. targetaudience.offer.api.GetRequest (offer 2001)
    let ta_rsp = quasar_rpc(&ctx, &mut sess, "eve_public.store.targetaudience.offer.api.GetRequest", make_ta_offer_request(2001)).await;
    assert_eq!(ta_rsp.status_code, STATUS_OK, "targetaudience offer 2001 failed: {}", ta_rsp.status_message);
    assert_eq!(ta_rsp.payload.as_ref().unwrap().normalized_type_name(), "eve_public.store.targetaudience.offer.api.GetResponse");

    let ta_miss_rsp = quasar_rpc(&ctx, &mut sess, "eve_public.store.targetaudience.offer.api.GetRequest", make_ta_offer_request(9999)).await;
    assert_eq!(ta_miss_rsp.status_code, STATUS_NOT_FOUND);
}

#[tokio::test]
async fn test_payment_schedules_tokens_and_user_license() {
    // DATA GAP: Payment purchase offer catalog (`eve_public/payment/purchase/api_pb2.pyj`) is external store data not in V24 client FSD.
    // parity: quasar:eveProto/generated/eve_public/payment/schedule/notices_pb2.pyj
    // parity: quasar:eveProto/generated/eve_public/payment/schedule/requests_pb2.pyj
    // parity: quasar:eveProto/generated/eve_public/payment/token/api/notices_pb2.pyj
    // parity: quasar:eveProto/generated/eve_public/payment/token/api_pb2.pyj
    // parity: quasar:eveProto/generated/eve_public/user/license/api/requests_pb2.pyj
    // parity: quasar:eveProto/generated/eve_public/user/license/api/notices_pb2.pyj
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    common::seed_test_nes_store_catalog(&ctx);
    let (uid, cid) = (1u64, 90_000_001);
    let mut sess = build_sess(3503, uid, cid);
    let mut notice_rx = ctx.quasar_gateway.subscribe_session(&sess, vec![], ctx.now_unix());

    // 1. User license before activation: inactive
    let lic_rsp = quasar_rpc(&ctx, &mut sess, "eve_public.user.license.api.GetRequest", vec![]).await;
    assert_eq!(lic_rsp.status_code, STATUS_OK);

    // 2. Activate Omega license for 30 days
    let now = ctx.now_unix();
    activate_user_omega_license(&ctx.db, &ctx.quasar_gateway.pending_notices, uid, 30 * 86400, now);
    let activated_grade = ctx.db.with_snapshot(|snap| {
        snap.characters.get(&(cid as i64)).map(|c| c.clone_grade).unwrap_or(0)
    });
    assert_eq!(activated_grade, 2, "Character clone_grade must be 2 (Omega) after activate_user_omega_license");
    flush_pending_notices(&ctx);
    let lic_notices = drain_notices(&mut notice_rx);
    assert!(lic_notices.iter().any(|n| n.payload.as_ref().map(|p| p.type_url.contains("user.license.api.ActivatedNotice")).unwrap_or(false)));

    let lic_act = quasar_rpc(&ctx, &mut sess, "eve_public.user.license.api.GetRequest", vec![]).await;
    assert_eq!(lic_act.status_code, STATUS_OK);
    let reg = eve_proto::quasar::QuasarDescriptorRegistry::open_default().expect("registry");
    let dyn_lic = reg.decode_dynamic("eve_public.user.license.api.GetResponse", &lic_act.payload.as_ref().unwrap().value).expect("decode license");
    if let Some(eve_proto::quasar::DynamicValue::Message(attr)) = dyn_lic.get(1) {
        assert!(attr.get(1).is_none() || attr.get(1) == Some(&eve_proto::quasar::DynamicValue::Bool(false)) || attr.get(1) == Some(&eve_proto::quasar::DynamicValue::Uint32(0)));
        if let Some(eve_proto::quasar::DynamicValue::Message(ts)) = attr.get(2) {
            match ts.get(1) {
                Some(eve_proto::quasar::DynamicValue::Int64(t)) => assert!(*t > now as i64),
                Some(eve_proto::quasar::DynamicValue::Uint64(t)) => assert!(*t > now),
                other => panic!("expected timestamp, got {other:?}"),
            }
        } else {
            panic!("missing expiry timestamp");
        }
    } else {
        panic!("missing license attributes");
    }

    // 3. Expire Omega license
    expire_user_omega_license(&ctx.db, &ctx.quasar_gateway.pending_notices, uid);
    let expired_grade = ctx.db.with_snapshot(|snap| {
        snap.characters.get(&(cid as i64)).map(|c| c.clone_grade).unwrap_or(0)
    });
    assert_eq!(expired_grade, 1, "Character clone_grade must be 1 (Alpha) after expire_user_omega_license");
    flush_pending_notices(&ctx);
    let exp_notices = drain_notices(&mut notice_rx);
    assert!(exp_notices.iter().any(|n| n.payload.as_ref().map(|p| p.type_url.contains("user.license.api.ExpiredNotice")).unwrap_or(false)));

    // 4. Payment tokens: register quick-pay token triggers QuickPayAddedNotice
    add_user_quick_pay_token(
        &ctx.db,
        &ctx.quasar_gateway.pending_notices,
        uid,
        PaymentTokenRecord {
            id: 1,
            user_id: uid,
            token_str: "tok_visa_4242".to_string(),
            is_quick_pay: true,
            consumed: false,
        },
    );
    flush_pending_notices(&ctx);
    let add_notices = drain_notices(&mut notice_rx);
    assert!(add_notices.iter().any(|n| n.payload.as_ref().map(|p| p.type_url.contains("QuickPayAddedNotice")).unwrap_or(false)));

    let tok_rsp = quasar_rpc(&ctx, &mut sess, "eve_public.payment.token.api.GetRequest", make_varint_id_request(1)).await;
    assert_eq!(tok_rsp.status_code, STATUS_OK, "payment.token.api.GetRequest failed: {}", tok_rsp.status_message);
    let tok_miss = quasar_rpc(&ctx, &mut sess, "eve_public.payment.token.api.GetRequest", make_varint_id_request(999)).await;
    assert_eq!(tok_miss.status_code, STATUS_NOT_FOUND);

    // Read-only GetQuickPayRequest must NOT emit any notice
    let qp_rsp = quasar_rpc(&ctx, &mut sess, "eve_public.payment.token.api.GetQuickPayRequest", vec![]).await;
    assert_eq!(qp_rsp.status_code, STATUS_OK);
    flush_pending_notices(&ctx);
    let get_notices = drain_notices(&mut notice_rx);
    assert!(get_notices.is_empty(), "GetQuickPayRequest must not emit notices");

    let dis_rsp = quasar_rpc(&ctx, &mut sess, "eve_public.payment.token.api.DisableAllRequest", vec![]).await;
    assert_eq!(dis_rsp.status_code, STATUS_OK);
    flush_pending_notices(&ctx);
    let tok_notices = drain_notices(&mut notice_rx);
    assert!(tok_notices.iter().any(|n| n.payload.as_ref().map(|p| p.type_url.contains("QuickPayRemovedNotice")).unwrap_or(false)));

    // 5. Payment schedules
    ctx.db.with_mut_snapshot(|snap| {
        let schedules = vec![PaymentScheduleRecord {
            id: 101,
            user_id: uid,
            schedule_type: 1,
            next_payment_unix: now + 30 * 86400,
            offer_id: "1001".to_string(),
            interval_days: 30,
            created_unix: now,
        }];
        snap.service_state_blobs.insert("payment_schedules".to_string(), serde_json::to_string(&schedules).unwrap());
    });
    let sched_rsp = quasar_rpc(&ctx, &mut sess, "eve_public.payment.schedule.api.GetRequest", make_varint_id_request(101)).await;
    assert_eq!(sched_rsp.status_code, STATUS_OK, "payment.schedule.api.GetRequest failed: {}", sched_rsp.status_message);

    let mut sched_type_payload = Vec::new();
    encode_varint_field(1, 1, &mut sched_type_payload);
    let sched_type = quasar_rpc(&ctx, &mut sess, "eve_public.payment.schedule.api.GetActiveByTypeRequest", sched_type_payload).await;
    assert_eq!(sched_type.status_code, STATUS_OK);

    // Cancel schedule 101 -> enqueues CancelledNotice
    let cancelled = cancel_user_payment_schedule(&ctx.db, &ctx.quasar_gateway.pending_notices, uid, 101);
    assert!(cancelled, "Schedule 101 should be cancelled successfully");
    flush_pending_notices(&ctx);
    let sched_notices = drain_notices(&mut notice_rx);
    assert!(sched_notices.iter().any(|n| n.payload.as_ref().map(|p| p.type_url.contains("CancelledNotice")).unwrap_or(false)));

    let sched_after = quasar_rpc(&ctx, &mut sess, "eve_public.payment.schedule.api.GetRequest", make_varint_id_request(101)).await;
    assert_eq!(sched_after.status_code, STATUS_NOT_FOUND);

    // 6. Payment purchase (CostRequest & TokenRequest)
    // Re-enable active quick pay tokens after DisableAllRequest and seed PLEX vault
    ctx.db.with_mut_snapshot(|snap| {
        snap.account_plex_vault.insert(uid, 1000);
        let tokens = vec![
            PaymentTokenRecord { id: 1, user_id: uid, token_str: "tok_visa_4242".into(), is_quick_pay: true, consumed: false },
            PaymentTokenRecord { id: 2, user_id: uid, token_str: "tok_visa_skin".into(), is_quick_pay: true, consumed: false },
        ];
        snap.service_state_blobs.insert("payment_tokens".to_string(), serde_json::to_string(&tokens).unwrap());
    });

    let mut cost_payload = Vec::new();
    encode_varint_field(1, 1001, &mut cost_payload);
    encode_string_field(2, "USD", &mut cost_payload);
    let cost_rsp = quasar_rpc(&ctx, &mut sess, "eve_public.payment.purchase.api.CostRequest", cost_payload).await;
    assert_eq!(cost_rsp.status_code, STATUS_OK, "CostRequest failed: {}", cost_rsp.status_message);

    let mut token_req_payload = Vec::new();
    let mut order_msg = Vec::new();
    let mut off_msg = Vec::new();
    encode_string_field(1, "1001", &mut off_msg);
    encode_bytes_field(1, &off_msg, &mut order_msg);
    encode_bytes_field(1, &order_msg, &mut token_req_payload);
    let mut tok_msg = Vec::new();
    encode_varint_field(1, 1, &mut tok_msg);
    encode_bytes_field(2, &tok_msg, &mut token_req_payload);

    // Negative assertion 1: sess_user2 attempting to use sess's token ID fails with STATUS_FORBIDDEN (403)
    let mut sess_user2 = build_sess(3598, 2, 90_000_002);
    let unowned_rsp = quasar_rpc(&ctx, &mut sess_user2, "eve_public.payment.purchase.api.TokenRequest", token_req_payload.clone()).await;
    assert_eq!(unowned_rsp.status_code, STATUS_FORBIDDEN, "Using another user's token must return STATUS_FORBIDDEN");

    // Happy path 1: User 1 uses valid unconsumed token 1 -> STATUS_OK (atomic Omega activation)
    let purchase_rsp = quasar_rpc(&ctx, &mut sess, "eve_public.payment.purchase.api.TokenRequest", token_req_payload.clone()).await;
    assert_eq!(purchase_rsp.status_code, STATUS_OK, "TokenRequest failed: {}", purchase_rsp.status_message);

    // Negative assertion 2: Calling TokenRequest a second time with the same consumed token ID fails with STATUS_BAD_REQUEST (400)
    let reused_rsp = quasar_rpc(&ctx, &mut sess, "eve_public.payment.purchase.api.TokenRequest", token_req_payload).await;
    assert_eq!(reused_rsp.status_code, STATUS_BAD_REQUEST, "Reusing consumed token must return STATUS_BAD_REQUEST");

    let (has_receipt, is_omega) = ctx.db.with_snapshot(|snap| {
        let receipts: Vec<eve_server::services::quasar_payment_store::PaymentReceiptRecord> = snap
            .service_state_blobs
            .get("payment_receipts")
            .and_then(|j| serde_json::from_str(j).ok())
            .unwrap_or_default();
        let omega = eve_server::services::quasar_auth_user::read_identity_store(&ctx.db, |st| st.omega_subscriptions.get(&uid).copied().unwrap_or(0));
        (receipts.iter().any(|r| r.user_id == uid && r.offer_id == "1001"), omega > now)
    });
    assert!(has_receipt, "Receipt must be recorded in payment_receipts");
    assert!(is_omega, "Offer 1001 must have activated Omega subscription");
    let token_purchase_grade = ctx.db.with_snapshot(|snap| {
        snap.characters.get(&(cid as i64)).map(|c| c.clone_grade).unwrap_or(0)
    });
    assert_eq!(token_purchase_grade, 2, "Character clone_grade must be 2 (Omega) after TokenRequest for Omega");

    // Happy path 2: User 1 uses token 2 for product offer 1002 (delivers SKIN 34599 into character hangar)
    let mut skin_req = Vec::new();
    let mut order_skin = Vec::new();
    let mut off_skin = Vec::new();
    encode_string_field(1, "1002", &mut off_skin);
    encode_bytes_field(1, &off_skin, &mut order_skin);
    encode_bytes_field(1, &order_skin, &mut skin_req);
    let mut tok2_msg = Vec::new();
    encode_varint_field(1, 2, &mut tok2_msg);
    encode_bytes_field(2, &tok2_msg, &mut skin_req);

    let skin_rsp = quasar_rpc(&ctx, &mut sess, "eve_public.payment.purchase.api.TokenRequest", skin_req).await;
    assert_eq!(skin_rsp.status_code, STATUS_OK, "TokenRequest for skin offer failed: {}", skin_rsp.status_message);
    let char_items = ctx.db.get_items_for_character(cid as i64);
    assert!(char_items.iter().any(|it| it.type_id == 34599), "Purchased SKIN 34599 must be delivered to character hangar");
}

#[tokio::test]
async fn test_quasar_achievements_progression_and_claims() {
    // DATA GAP: Achievement categories, showcase scores, and milestone rewards are server-only and not present in res:/staticdata/achievements.static.
    // parity: quasar:eveProto/generated/eve_public/achievement/api/requests_pb2.pyj
    // parity: quasar:eveProto/generated/eve_public/achievement/api/admin/requests_pb2.pyj
    // parity: quasar:eveProto/generated/eve_public/achievement/api/notices_pb2.pyj
    // parity: quasar:eveProto/generated/eve_public/achievement/definition/api/cdn/requests_pb2.pyj
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let (uid, cid) = (1u64, 90_000_001);
    let mut sess = build_sess(3504, uid, cid);
    let mut notice_rx = ctx.quasar_gateway.subscribe_session(&sess, vec![], ctx.now_unix());

    // 1. Definition CDN query
    let cdn_rsp = quasar_rpc(&ctx, &mut sess, "eve_public.achievement.definition.api.cdn.GetLatestRequest", vec![]).await;
    assert_eq!(cdn_rsp.status_code, STATUS_OK);
    assert_eq!(cdn_rsp.payload.as_ref().unwrap().normalized_type_name(), "eve_public.achievement.definition.api.cdn.GetLatestResponse");

    // 2. Category query: Category 1 (Combat)
    let cat_rsp = quasar_rpc(&ctx, &mut sess, "eve_public.achievement.category.api.GetRequest", make_id_request(1)).await;
    assert_eq!(cat_rsp.status_code, STATUS_OK, "Category GetRequest failed: {}", cat_rsp.status_message);
    assert_eq!(cat_rsp.payload.as_ref().unwrap().normalized_type_name(), "eve_public.achievement.category.api.GetResponse");

    // 3. Showcase score query before progression
    let score_rsp = quasar_rpc(&ctx, &mut sess, "eve_public.achievement.showcase.api.GetCharacterScoreRequest", vec![]).await;
    assert_eq!(score_rsp.status_code, STATUS_OK);
    assert_eq!(score_rsp.payload.as_ref().unwrap().normalized_type_name(), "eve_public.achievement.showcase.api.GetCharacterScoreResponse");

    // 4. Achievement query: 2, default empty query, and negative tests
    let ach_rsp = quasar_rpc(&ctx, &mut sess, "eve_public.achievement.api.GetRequest", make_id_request(2)).await;
    assert_eq!(ach_rsp.status_code, STATUS_OK, "achievement 2 failed: {}", ach_rsp.status_message);
    assert_eq!(ach_rsp.payload.as_ref().unwrap().normalized_type_name(), "eve_public.achievement.api.GetResponse");

    let empty_ach = quasar_rpc(&ctx, &mut sess, "eve_public.achievement.api.GetRequest", vec![]).await;
    assert_eq!(empty_ach.status_code, STATUS_BAD_REQUEST);

    let miss_ach = quasar_rpc(&ctx, &mut sess, "eve_public.achievement.api.GetRequest", make_id_request(99999)).await;
    assert_eq!(miss_ach.status_code, STATUS_NOT_FOUND);

    // 5. Admin AddProgress: add 10 to reach milestone 10
    let prog_rsp = quasar_rpc(&ctx, &mut sess, "eve_public.achievement.api.admin.AddProgressRequest", make_admin_progress(2, 10)).await;
    assert_eq!(prog_rsp.status_code, STATUS_OK, "AddProgressRequest failed: {}", prog_rsp.status_message);
    assert_eq!(prog_rsp.payload.as_ref().unwrap().normalized_type_name(), "google.protobuf.Empty");

    flush_pending_notices(&ctx);
    let ach_notices = drain_notices(&mut notice_rx);
    assert!(ach_notices.iter().any(|n| n.payload.as_ref().map(|p| p.type_url.contains("achievement.api.ProgressedNotice")).unwrap_or(false)));

    // 6. Reward GetUnclaimedRequest -> has unclaimed rewards
    let unclaim_rsp = quasar_rpc(&ctx, &mut sess, "eve_public.achievement.reward.api.GetUnclaimedRequest", vec![]).await;
    assert_eq!(unclaim_rsp.status_code, STATUS_OK);
    assert_eq!(unclaim_rsp.payload.as_ref().unwrap().normalized_type_name(), "eve_public.achievement.reward.api.GetUnclaimedResponse");

    // 7. Claim reward: ClaimRequest
    let claim_rsp = quasar_rpc(&ctx, &mut sess, "eve_public.achievement.reward.api.ClaimRequest", vec![]).await;
    assert_eq!(claim_rsp.status_code, STATUS_OK, "ClaimRequest failed: {}", claim_rsp.status_message);
    assert_eq!(claim_rsp.payload.as_ref().unwrap().normalized_type_name(), "google.protobuf.Empty");

    // Verify WalletJournalEntry created for character
    let has_journal = ctx.db.with_snapshot(|snap| {
        snap.wallet_journal.iter().any(|j| j.character_id == cid as i64 && j.entry_type == "achievement_reward")
    });
    assert!(has_journal, "Expected WalletJournalEntry for claimed achievement reward");

    // 8. Admin CompleteRequest -> completes achievement 2
    let comp_rsp = quasar_rpc(&ctx, &mut sess, "eve_public.achievement.api.admin.CompleteRequest", make_id_request(2)).await;
    assert_eq!(comp_rsp.status_code, STATUS_OK, "CompleteRequest failed: {}", comp_rsp.status_message);
    assert_eq!(comp_rsp.payload.as_ref().unwrap().normalized_type_name(), "google.protobuf.Empty");

    // Claim rewards unlocked by CompleteRequest so unclaimed_reward_count returns to 0
    let claim2_rsp = quasar_rpc(&ctx, &mut sess, "eve_public.achievement.reward.api.ClaimRequest", vec![]).await;
    assert_eq!(claim2_rsp.status_code, STATUS_OK);

    flush_pending_notices(&ctx);
    let comp_notices = drain_notices(&mut notice_rx);
    assert!(comp_notices.iter().any(|n| n.payload.as_ref().map(|p| p.type_url.contains("achievement.api.CompletedNotice")).unwrap_or(false)));

    // 9. Showcase score query reflects updated points and cross-character score query
    let score2_rsp = quasar_rpc(&ctx, &mut sess, "eve_public.achievement.showcase.api.GetCharacterScoreRequest", vec![]).await;
    assert_eq!(score2_rsp.status_code, STATUS_OK);
    assert_eq!(score2_rsp.payload.as_ref().unwrap().normalized_type_name(), "eve_public.achievement.showcase.api.GetCharacterScoreResponse");

    let mut other_sess = SessionState::new(99_001);
    other_sess.user_id = Some(2);
    other_sess.character_id = Some(90_000_002);
    let mut char_ident = Vec::new();
    encode_varint_field(1, cid as u64, &mut char_ident);
    let mut cross_body = Vec::new();
    encode_bytes_field(1, &char_ident, &mut cross_body);
    let cross_rsp = quasar_rpc(&ctx, &mut other_sess, "eve_public.achievement.showcase.api.GetCharacterScoreRequest", cross_body).await;
    assert_eq!(cross_rsp.status_code, STATUS_OK);
    assert_eq!(cross_rsp.payload.as_ref().unwrap().value, score2_rsp.payload.as_ref().unwrap().value);

    // 10. SQLite persistence save-and-reload round-trip:
    // Assert PLEX vault balance, GetLogRequest transaction record, and claimed achievement state persist
    ctx.db.with_mut_snapshot(|snap| {
        snap.account_plex_vault.insert(uid, 750);
        eve_server::services::quasar_payment_store::record_plex_transaction(
            snap, 888, uid, 750, 750, "SQLite Persistence Test", ctx.now_unix(),
        );
    });

    let tmp_db = std::env::temp_dir().join(format!("evomu_store_vault_roundtrip_{}.sqlite", std::process::id()));
    let _ = std::fs::remove_file(&tmp_db);
    ctx.db.save_to_sqlite(&tmp_db).expect("save_to_sqlite");
    let ctx_reloaded = ServerContext::from_sqlite(&tmp_db).expect("from_sqlite");

    // 10a. Assert PLEX vault balance persists across SQLite reload
    let reloaded_plex = ctx_reloaded.db.with_snapshot(|snap| {
        snap.account_plex_vault.get(&uid).copied().unwrap_or(0)
    });
    assert_eq!(reloaded_plex, 750, "PLEX vault balance must persist across SQLite reload");

    // 10b. Assert GetLogRequest transaction record persists across SQLite reload
    let mut sess_reloaded = build_sess(3599, uid, cid);
    let log_rsp = quasar_rpc(&ctx_reloaded, &mut sess_reloaded, "eve_public.plex.vault.transaction.api.GetLogRequest", make_varint_id_request(888)).await;
    assert_eq!(log_rsp.status_code, STATUS_OK, "Transaction 888 must be found on reloaded server context");

    // 10c. Assert claimed achievement state persists across SQLite reload
    let mgr_reloaded = eve_server::services::quasar_achievements::AchievementManager::new(Some(std::sync::Arc::clone(&ctx_reloaded.db)));
    let achs_reloaded = mgr_reloaded.get_or_init_achievements(cid);
    let a2_reloaded = achs_reloaded.iter().find(|a| a.def.id == 2).expect("a2 on reloaded ctx");
    assert!(a2_reloaded.completed, "Achievement 2 completed state must persist across SQLite reload");
    assert_eq!(a2_reloaded.unclaimed_reward_count, 0, "Achievement 2 unclaimed reward count must remain 0 across SQLite reload");

    let _ = std::fs::remove_file(&tmp_db);
    let _ = std::fs::remove_file(tmp_db.with_extension("sqlite-wal"));
    let _ = std::fs::remove_file(tmp_db.with_extension("sqlite-shm"));

    // 11. Backward compatibility: load legacy float JSON (reward_isk: 250000.0, reward_sp: 5000.0)
    let old_float_json = serde_json::json!({
        "90000001": [
            {
                "def": {
                    "id": 101,
                    "category_id": 1,
                    "name": "Destroy Hostile Vessels",
                    "target": 100,
                    "score": 250,
                    "milestone_targets": [10, 50, 100],
                    "reward_isk": 250000.0,
                    "reward_sp": 5000.0
                },
                "progress": 5,
                "completed": false,
                "completed_date": null,
                "reached_milestones": [],
                "unclaimed_reward_count": 0
            }
        ]
    }).to_string();
    ctx.db.with_mut_snapshot(|s| {
        s.quasar_state_blobs.insert("quasar_achievements".into(), old_float_json);
    });
    let mgr = eve_server::services::quasar_achievements::AchievementManager::new(Some(std::sync::Arc::clone(&ctx.db)));
    let achs = mgr.get_or_init_achievements(cid);
    let a101 = achs.iter().find(|a| a.def.id == 101).expect("a101 loaded");
    assert_eq!(a101.progress, 5);
    assert_eq!(a101.def.reward_isk, 250_000);
    assert_eq!(a101.def.reward_sp, 5_000);
}
