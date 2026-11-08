//! Unit tests for `quasar_payment_store` PLEX vault, store catalog/offers, fulfillment,
//! payment schedules, tokens, purchases, and PLEX transaction logs.

#![forbid(unsafe_code)]

use super::*;
use crate::quasar_gateway::{
    PublicGatewayRouter, STATUS_BAD_REQUEST, STATUS_FORBIDDEN, STATUS_NOT_FOUND,
    STATUS_NOT_IMPLEMENTED, STATUS_OK,
};
use eve_db::WorldRepository;
use eve_machonet::{service_roles, SessionState};
use eve_proto::quasar::{
    encode_bytes_field, encode_string_field, encode_varint_field, encode_zigzag64,
    EvePublicRequest, EvePublicResponse, GoogleAny, GoogleTimestamp, QuasarDescriptorRegistry,
    TYPE_GOOGLEAPIS_PREFIX,
};
use eve_sde::SdeRegistry;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

fn new_router() -> PublicGatewayRouter {
    PublicGatewayRouter::new(
        QuasarDescriptorRegistry::open_default_or_empty(),
        Arc::new(WorldRepository::with_demo_seed()),
        shared_sde(),
    )
}

fn make_sess(uid: Option<u64>, cid: Option<i64>, role: u64) -> SessionState {
    let mut s = SessionState::new(uid.unwrap_or(1));
    s.user_id = uid;
    s.character_id = cid;
    s.corporation_id = Some(98_000_001);
    s.solar_system_id = Some(30_000_142);
    s.role = role;
    s
}

fn call(router: &PublicGatewayRouter, sess: &SessionState, type_name: &str, value: Vec<u8>, now: u64) -> EvePublicResponse {
    let req = EvePublicRequest {
        issued: Some(GoogleTimestamp { seconds: now as i64, nanos: 0 }),
        correlation_uuid: vec![1; 16],
        payload: Some(GoogleAny {
            type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{type_name}"),
            value,
        }),
        external_origin: "test".into(),
        application_instance_uuid: Vec::new(),
        authoritative_context: None,
        trace_context: None,
    };
    router.handle_public_request(sess, &req, None, false, now)
}

fn seed_catalog(db: &WorldRepository) {
    db.with_mut_snapshot(|snap| {
        let cat = StoreCatalog {
            categories: vec![
                StoreCategory { id: 1, name: "Omega".into(), description: "Omega sub".into() },
                StoreCategory { id: 2, name: "SKINs".into(), description: "Ship SKINs".into() },
            ],
            products: vec![
                StoreProduct { id: 101, name: "30d Omega".into(), description: "Omega".into(), type_id: 0, quantity: 1 },
                StoreProduct { id: 102, name: "Test SKIN".into(), description: "SKIN".into(), type_id: 34599, quantity: 2 },
            ],
            offers: vec![
                StoreOffer {
                    id: 1001,
                    name: "30 Days Omega".into(),
                    description: "Omega time".into(),
                    category_id: 1,
                    price_plex: 500,
                    product_ids: vec![101],
                    targeted: false,
                    price_cents: Some(1_999),
                    currency: Some("USD".into()),
                },
                StoreOffer {
                    id: 1002,
                    name: "Rifter SKIN".into(),
                    description: "Public SKIN".into(),
                    category_id: 2,
                    price_plex: 110,
                    product_ids: vec![102],
                    targeted: false,
                    price_cents: Some(499),
                    currency: Some("EUR".into()),
                },
                StoreOffer {
                    id: 1003,
                    name: "NoCashOffer".into(),
                    description: "PLEX only".into(),
                    category_id: 2,
                    price_plex: 50,
                    product_ids: vec![102],
                    targeted: true,
                    price_cents: None,
                    currency: None,
                },
                StoreOffer {
                    id: 1004,
                    name: "TargetedDeal2".into(),
                    description: "Targeted 2".into(),
                    category_id: 2,
                    price_plex: 75,
                    product_ids: vec![102],
                    targeted: true,
                    price_cents: Some(299),
                    currency: Some("USD".into()),
                },
            ],
        };
        save_store_catalog(snap, &cat);
        save_store_offer_tags(snap, &HashMap::from([(1002u32, vec!["featured".into(), "hot".into()])]));
    });
}

fn encode_currency_field(field: u32, cents: i64, out: &mut Vec<u8>) {
    let mut c = Vec::new();
    encode_varint_field(1, encode_zigzag64(cents), &mut c);
    encode_bytes_field(field, &c, out);
}

#[test]
fn plex_vault_balance_deposit_withdraw_and_tx_logs() {
    let router = new_router();
    let uid = 42u64;
    let player = make_sess(Some(uid), Some(90_000_001), service_roles::ROLE_PLAYER_DEFAULT);
    let gm = make_sess(Some(uid), Some(90_000_001), service_roles::ROLE_GM_DEFAULT);
    let now = 1_750_000_000u64;

    // Balance starts at 0 for fresh user 42.
    let rsp_bal0 = call(&router, &player, "eve_public.plex.vault.api.BalanceRequest", Vec::new(), now);
    assert_eq!(rsp_bal0.status_code, STATUS_OK);

    // Non-GM deposit/withdraw forbidden (403).
    assert_eq!(
        call(&router, &player, "eve.plex.vault.assetholding.operation.api.DepositRequest", Vec::new(), now).status_code,
        STATUS_FORBIDDEN
    );
    assert_eq!(
        call(&router, &player, "eve.plex.vault.assetholding.operation.api.WithdrawRequest", Vec::new(), now).status_code,
        STATUS_FORBIDDEN
    );

    // Zero deposit/withdraw rejected (400).
    let mut zero_req = Vec::new();
    encode_currency_field(3, 0, &mut zero_req);
    assert_eq!(
        call(&router, &gm, "eve.plex.vault.assetholding.operation.api.DepositRequest", zero_req.clone(), now).status_code,
        STATUS_BAD_REQUEST
    );
    assert_eq!(
        call(&router, &gm, "eve.plex.vault.assetholding.operation.api.WithdrawRequest", zero_req, now).status_code,
        STATUS_BAD_REQUEST
    );

    // Withdraw before deposit fails with insufficient PLEX balance (400).
    let mut w_fail = Vec::new();
    encode_currency_field(3, 5_000, &mut w_fail);
    assert_eq!(
        call(&router, &gm, "eve.plex.vault.assetholding.operation.api.WithdrawRequest", w_fail, now).status_code,
        STATUS_BAD_REQUEST
    );

    // Deposit via field 3 (+250 PLEX), field 2 (+150 PLEX), and default (+100 PLEX) -> 500 PLEX.
    let mut dep1 = Vec::new();
    encode_currency_field(3, 25_000, &mut dep1);
    assert_eq!(
        call(&router, &gm, "eve.plex.vault.assetholding.operation.api.DepositRequest", dep1, now).status_code,
        STATUS_OK
    );
    let mut dep2 = Vec::new();
    encode_currency_field(2, 15_000, &mut dep2);
    assert_eq!(
        call(&router, &gm, "eve.plex.vault.assetholding.operation.api.DepositRequest", dep2, now + 1).status_code,
        STATUS_OK
    );
    assert_eq!(
        call(&router, &gm, "eve.plex.vault.assetholding.operation.api.DepositRequest", Vec::new(), now + 2).status_code,
        STATUS_OK
    );

    // Withdraw via field 3 (-80 PLEX), field 2 (-20 PLEX), and default (-50 PLEX) -> 350 PLEX remaining.
    let mut w1 = Vec::new();
    encode_currency_field(3, 8_000, &mut w1);
    assert_eq!(
        call(&router, &gm, "eve.plex.vault.assetholding.operation.api.WithdrawRequest", w1, now + 3).status_code,
        STATUS_OK
    );
    let mut w2 = Vec::new();
    encode_currency_field(2, 2_000, &mut w2);
    assert_eq!(
        call(&router, &gm, "eve.plex.vault.assetholding.operation.api.WithdrawRequest", w2, now + 4).status_code,
        STATUS_OK
    );
    assert_eq!(
        call(&router, &gm, "eve.plex.vault.assetholding.operation.api.WithdrawRequest", Vec::new(), now + 5).status_code,
        STATUS_OK
    );

    let final_bal = router.db.with_snapshot(|s| s.account_plex_vault.get(&uid).copied().unwrap_or(0));
    assert_eq!(final_bal, 270);

    // Transaction logs & statistics
    let rsp_all = call(&router, &player, "eve_public.plex.vault.transaction.api.GetAllLoggedForUserRequest", Vec::new(), now + 6);
    assert_eq!(rsp_all.status_code, STATUS_OK);

    let tx_id = router.db.with_snapshot(|s| get_plex_transactions(s)[0].id);
    assert_eq!(
        call(&router, &player, "eve_public.plex.vault.transaction.api.GetLogRequest", Vec::new(), now + 6).status_code,
        STATUS_BAD_REQUEST
    );
    let mut bad_tx = Vec::new();
    encode_varint_field(1, 999_999, &mut bad_tx);
    assert_eq!(
        call(&router, &player, "eve_public.plex.vault.transaction.api.GetLogRequest", bad_tx, now + 6).status_code,
        STATUS_NOT_FOUND
    );
    let mut ok_tx = Vec::new();
    encode_varint_field(1, tx_id, &mut ok_tx);
    assert_eq!(
        call(&router, &player, "eve_public.plex.vault.transaction.api.GetLogRequest", ok_tx, now + 6).status_code,
        STATUS_OK
    );

    let rsp_stats = call(&router, &player, "eve_public.plex.vault.transaction.api.GetStatisticsRequest", Vec::new(), now + 6);
    assert_eq!(rsp_stats.status_code, STATUS_OK);
}

#[test]
fn store_catalog_offers_targeted_pagination_and_redeem_code() {
    let router = new_router();
    seed_catalog(&router.db);
    let player = make_sess(Some(10), None, service_roles::ROLE_PLAYER_DEFAULT);
    let now = 1_750_000_100u64;

    assert_eq!(call(&router, &player, "eve_public.store.category.api.GetAllRequest", Vec::new(), now).status_code, STATUS_OK);
    assert_eq!(call(&router, &player, "eve_public.store.category.api.GetRequest", Vec::new(), now).status_code, STATUS_BAD_REQUEST);
    let mut cat_99_inner = Vec::new();
    encode_varint_field(1, 99, &mut cat_99_inner);
    let mut cat_99 = Vec::new();
    encode_bytes_field(1, &cat_99_inner, &mut cat_99);
    assert_eq!(call(&router, &player, "eve_public.store.category.api.GetRequest", cat_99, now).status_code, STATUS_NOT_FOUND);
    let mut cat_1_inner = Vec::new();
    encode_varint_field(1, 1, &mut cat_1_inner);
    let mut cat_1 = Vec::new();
    encode_bytes_field(1, &cat_1_inner, &mut cat_1);
    assert_eq!(call(&router, &player, "eve_public.store.category.api.GetRequest", cat_1, now).status_code, STATUS_OK);

    assert_eq!(call(&router, &player, "eve_public.store.product.api.GetRequest", Vec::new(), now).status_code, STATUS_BAD_REQUEST);
    let mut prod_99_inner = Vec::new();
    encode_varint_field(1, 99, &mut prod_99_inner);
    let mut prod_99 = Vec::new();
    encode_bytes_field(1, &prod_99_inner, &mut prod_99);
    assert_eq!(call(&router, &player, "eve_public.store.product.api.GetRequest", prod_99, now).status_code, STATUS_NOT_FOUND);
    let mut prod_102_inner = Vec::new();
    encode_varint_field(1, 102, &mut prod_102_inner);
    let mut prod_102 = Vec::new();
    encode_bytes_field(1, &prod_102_inner, &mut prod_102);
    assert_eq!(call(&router, &player, "eve_public.store.product.api.GetRequest", prod_102, now).status_code, STATUS_OK);

    assert_eq!(call(&router, &player, "eve_public.store.offer.api.GetAllRequest", Vec::new(), now).status_code, STATUS_OK);
    assert_eq!(call(&router, &player, "eve_public.store.offer.api.GetRequest", Vec::new(), now).status_code, STATUS_BAD_REQUEST);
    let mut off_9999_id = Vec::new();
    encode_string_field(1, "9999", &mut off_9999_id);
    let mut off_9999 = Vec::new();
    encode_bytes_field(1, &off_9999_id, &mut off_9999);
    assert_eq!(call(&router, &player, "eve_public.store.offer.api.GetRequest", off_9999, now).status_code, STATUS_NOT_FOUND);
    let mut off_1002_id = Vec::new();
    encode_string_field(1, "1002", &mut off_1002_id);
    let mut off_1002 = Vec::new();
    encode_bytes_field(1, &off_1002_id, &mut off_1002);
    assert_eq!(call(&router, &player, "eve_public.store.offer.api.GetRequest", off_1002, now).status_code, STATUS_OK);

    assert_eq!(call(&router, &player, "eve_public.store.offer.api.GetAllTargetedRequest", Vec::new(), now).status_code, STATUS_OK);

    // Targeted offer pagination (page_size=1, offset="0" -> next_page="1")
    let mut page_req = Vec::new();
    let mut page_sub = Vec::new();
    encode_varint_field(1, 1, &mut page_sub);
    encode_string_field(2, "0", &mut page_sub);
    encode_bytes_field(1, &page_sub, &mut page_req);
    let rsp_ta_list = call(&router, &player, "eve_public.store.targetaudience.offer.api.ListRequest", page_req, now);
    assert_eq!(rsp_ta_list.status_code, STATUS_OK);

    // Targeted GetRequest: missing -> 400, unknown -> 404, valid nested field 2 -> 200
    assert_eq!(call(&router, &player, "eve_public.store.targetaudience.offer.api.GetRequest", Vec::new(), now).status_code, STATUS_BAD_REQUEST);
    let mut ta_entry_bad = Vec::new();
    encode_string_field(1, "9999", &mut ta_entry_bad);
    let mut ta_id_bad = Vec::new();
    encode_bytes_field(2, &ta_entry_bad, &mut ta_id_bad);
    let mut ta_req_bad = Vec::new();
    encode_bytes_field(1, &ta_id_bad, &mut ta_req_bad);
    assert_eq!(call(&router, &player, "eve_public.store.targetaudience.offer.api.GetRequest", ta_req_bad, now).status_code, STATUS_NOT_FOUND);

    let mut ta_entry_ok = Vec::new();
    encode_string_field(1, "1003", &mut ta_entry_ok);
    let mut ta_id_ok = Vec::new();
    encode_bytes_field(2, &ta_entry_ok, &mut ta_id_ok);
    let mut ta_req_ok = Vec::new();
    encode_bytes_field(1, &ta_id_ok, &mut ta_req_ok);
    assert_eq!(call(&router, &player, "eve_public.store.targetaudience.offer.api.GetRequest", ta_req_ok, now).status_code, STATUS_OK);

    // Code redeem is unimplemented (501)
    assert_eq!(call(&router, &player, "eve_public.payment.code.api.RedeemRequest", Vec::new(), now).status_code, STATUS_NOT_IMPLEMENTED);
}

#[test]
fn schedules_tokens_fulfillment_cost_and_token_purchase() {
    let router = new_router();
    seed_catalog(&router.db);
    let uid = 20u64;
    let cid = 90_000_020i64;
    router.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().expect("demo char").clone();
        ch.character_id = cid;
        ch.user_id = uid;
        ch.station_id = Some(60_003_760);
        snap.characters.insert(cid, ch);

        let mut ch_nostation = snap.characters.values().next().expect("demo char").clone();
        ch_nostation.character_id = 90_000_099;
        ch_nostation.user_id = 99;
        ch_nostation.station_id = None;
        ch_nostation.structure_id = None;
        ch_nostation.home_station_id = 0;
        snap.characters.insert(90_000_099, ch_nostation);
    });

    let player = make_sess(Some(uid), Some(cid), service_roles::ROLE_PLAYER_DEFAULT);
    let other_player = make_sess(Some(99), Some(90_000_099), service_roles::ROLE_PLAYER_DEFAULT);
    let gm = make_sess(Some(uid), Some(cid), service_roles::ROLE_GM_DEFAULT);
    let now = 1_750_000_200u64;

    // 1. Fulfillment: non-GM -> 403, qty <= 0 -> 400, no station -> 400, valid -> delivers & stacks!
    assert_eq!(call(&router, &player, "eve.fulfillment.api.DeliverItemRequest", Vec::new(), now).status_code, STATUS_FORBIDDEN);
    let mut bad_qty = Vec::new();
    encode_varint_field(3, 0, &mut bad_qty);
    assert_eq!(call(&router, &gm, "eve.fulfillment.api.DeliverItemRequest", bad_qty, now).status_code, STATUS_BAD_REQUEST);

    let mut no_station_recip = Vec::new();
    encode_varint_field(1, encode_zigzag64(90_000_099), &mut no_station_recip);
    let mut no_station_req = Vec::new();
    encode_bytes_field(1, &no_station_recip, &mut no_station_req);
    assert_eq!(call(&router, &gm, "eve.fulfillment.api.DeliverItemRequest", no_station_req, now).status_code, STATUS_BAD_REQUEST);

    let mut deliver_recip = Vec::new();
    encode_varint_field(1, encode_zigzag64(cid), &mut deliver_recip);
    let mut deliver_req = Vec::new();
    encode_bytes_field(1, &deliver_recip, &mut deliver_req);
    let mut item_type = Vec::new();
    encode_varint_field(1, 34599, &mut item_type);
    encode_bytes_field(2, &item_type, &mut deliver_req);
    encode_varint_field(3, 3, &mut deliver_req);
    assert_eq!(call(&router, &gm, "eve.fulfillment.api.DeliverItemRequest", deliver_req.clone(), now).status_code, STATUS_OK);
    // Second delivery stacks onto the existing hangar stack (3 + 3 = 6).
    assert_eq!(call(&router, &gm, "eve.fulfillment.api.DeliverItemRequest", deliver_req, now).status_code, STATUS_OK);
    let delivered_qty: i32 = router.db.with_snapshot(|s| {
        s.items.values().filter(|i| i.owner_id == cid && i.type_id == 34599).map(|i| i.quantity).sum()
    });
    assert_eq!(delivered_qty, 6);

    // 2. CostRequest: missing -> 400, unknown -> 404, no price_cents (1003) -> 501, valid (1001) -> 200
    assert_eq!(call(&router, &player, "eve_public.payment.purchase.api.CostRequest", Vec::new(), now).status_code, STATUS_BAD_REQUEST);
    let mut cost_9999 = Vec::new();
    encode_varint_field(1, 9999, &mut cost_9999);
    assert_eq!(call(&router, &player, "eve_public.payment.purchase.api.CostRequest", cost_9999, now).status_code, STATUS_NOT_FOUND);
    let mut cost_1003 = Vec::new();
    encode_varint_field(1, 1003, &mut cost_1003);
    assert_eq!(call(&router, &player, "eve_public.payment.purchase.api.CostRequest", cost_1003, now).status_code, STATUS_NOT_IMPLEMENTED);
    let mut cost_1001 = Vec::new();
    encode_varint_field(1, 1001, &mut cost_1001);
    assert_eq!(call(&router, &player, "eve_public.payment.purchase.api.CostRequest", cost_1001, now).status_code, STATUS_OK);

    // 3. Payment tokens & TokenRequest purchase (Omega + item delivery + negative token checks)
    add_user_quick_pay_token(
        &router.db,
        &router.pending_notices,
        uid,
        PaymentTokenRecord { id: 501, user_id: uid, token_str: "tok-qp".into(), is_quick_pay: true, consumed: false },
    );
    add_user_quick_pay_token(
        &router.db,
        &router.pending_notices,
        uid,
        PaymentTokenRecord { id: 502, user_id: uid, token_str: "tok-noqp".into(), is_quick_pay: false, consumed: false },
    );

    assert_eq!(call(&router, &player, "eve_public.payment.token.api.GetQuickPayRequest", Vec::new(), now).status_code, STATUS_OK);
    assert_eq!(call(&router, &player, "eve_public.payment.token.api.GetRequest", Vec::new(), now).status_code, STATUS_BAD_REQUEST);
    let mut tok_999 = Vec::new();
    encode_varint_field(1, 999, &mut tok_999);
    let mut tok_get_999 = Vec::new();
    encode_bytes_field(1, &tok_999, &mut tok_get_999);
    assert_eq!(call(&router, &player, "eve_public.payment.token.api.GetRequest", tok_get_999, now).status_code, STATUS_NOT_FOUND);
    let mut tok_501 = Vec::new();
    encode_varint_field(1, 501, &mut tok_501);
    let mut tok_get_501 = Vec::new();
    encode_bytes_field(1, &tok_501, &mut tok_get_501);
    assert_eq!(call(&router, &player, "eve_public.payment.token.api.GetRequest", tok_get_501, now).status_code, STATUS_OK);

    let encode_tok_purchase = |off: &str, tid: u64| -> Vec<u8> {
        let mut req = Vec::new();
        let mut order = Vec::new();
        let mut oid = Vec::new();
        encode_string_field(1, off, &mut oid);
        encode_bytes_field(1, &oid, &mut order);
        encode_varint_field(2, 1, &mut order);
        encode_bytes_field(1, &order, &mut req);
        let mut tok = Vec::new();
        encode_varint_field(1, tid, &mut tok);
        encode_bytes_field(2, &tok, &mut req);
        req
    };

    // Other user trying to use token 501 gets 403 Forbidden; non-quick-pay token 502 gets 400 Bad Request.
    let tok_req_501 = encode_tok_purchase("1001", 501);
    assert_eq!(call(&router, &other_player, "eve_public.payment.purchase.api.TokenRequest", tok_req_501.clone(), now).status_code, STATUS_FORBIDDEN);

    let tok_req_502 = encode_tok_purchase("1001", 502);
    assert_eq!(call(&router, &player, "eve_public.payment.purchase.api.TokenRequest", tok_req_502, now).status_code, STATUS_BAD_REQUEST);

    // Purchase Omega (1001) with token 501 -> succeeds, activates Omega, creates schedule, marks 501 consumed!
    assert_eq!(call(&router, &player, "eve_public.payment.purchase.api.TokenRequest", tok_req_501.clone(), now).status_code, STATUS_OK);
    // Reusing consumed token 501 fails (400).
    assert_eq!(call(&router, &player, "eve_public.payment.purchase.api.TokenRequest", tok_req_501, now).status_code, STATUS_BAD_REQUEST);

    // Purchase item offer (1002, qty=2) with fresh token 503 -> delivers 2*2=4 items to hangar!
    add_user_quick_pay_token(
        &router.db,
        &router.pending_notices,
        uid,
        PaymentTokenRecord { id: 503, user_id: uid, token_str: "tok-item".into(), is_quick_pay: true, consumed: false },
    );
    let mut tok_req_503 = Vec::new();
    let mut order_msg = Vec::new();
    let mut off_id_msg = Vec::new();
    encode_string_field(1, "1002", &mut off_id_msg);
    encode_bytes_field(1, &off_id_msg, &mut order_msg);
    encode_varint_field(2, 2, &mut order_msg);
    encode_bytes_field(1, &order_msg, &mut tok_req_503);
    let mut tok_503_msg = Vec::new();
    encode_varint_field(1, 503, &mut tok_503_msg);
    encode_bytes_field(2, &tok_503_msg, &mut tok_req_503);
    assert_eq!(call(&router, &player, "eve_public.payment.purchase.api.TokenRequest", tok_req_503, now).status_code, STATUS_OK);

    let after_purchase_qty: i32 = router.db.with_snapshot(|s| {
        assert_eq!(s.characters[&cid].clone_grade, 2);
        s.items.values().filter(|i| i.owner_id == cid && i.type_id == 34599).map(|i| i.quantity).sum()
    });
    assert_eq!(after_purchase_qty, 10);

    // 4. Schedule GetActiveByTypeRequest & GetRequest + DisableAllRequest
    let mut active_sched_req = Vec::new();
    encode_varint_field(1, 1, &mut active_sched_req);
    assert_eq!(call(&router, &player, "eve_public.payment.schedule.api.GetActiveByTypeRequest", active_sched_req.clone(), now).status_code, STATUS_OK);
    assert_eq!(call(&router, &other_player, "eve_public.payment.schedule.api.GetActiveByTypeRequest", active_sched_req, now).status_code, STATUS_NOT_FOUND);

    let sched_id = router.db.with_snapshot(|s| s.quasar_commerce.payment_schedules[0].id);
    assert_eq!(call(&router, &player, "eve_public.payment.schedule.api.GetRequest", Vec::new(), now).status_code, STATUS_BAD_REQUEST);
    let mut sched_id_msg = Vec::new();
    encode_varint_field(1, sched_id, &mut sched_id_msg);
    let mut sched_req = Vec::new();
    encode_bytes_field(1, &sched_id_msg, &mut sched_req);
    assert_eq!(call(&router, &player, "eve_public.payment.schedule.api.GetRequest", sched_req.clone(), now).status_code, STATUS_OK);
    assert_eq!(call(&router, &other_player, "eve_public.payment.schedule.api.GetRequest", sched_req, now).status_code, STATUS_NOT_FOUND);

    add_user_quick_pay_token(
        &router.db,
        &router.pending_notices,
        uid,
        PaymentTokenRecord { id: 504, user_id: uid, token_str: "tok-dis".into(), is_quick_pay: true, consumed: false },
    );
    assert_eq!(call(&router, &player, "eve_public.payment.token.api.DisableAllRequest", Vec::new(), now).status_code, STATUS_OK);
    let any_qp = router.db.with_snapshot(|s| {
        s.quasar_commerce
            .payment_tokens
            .iter()
            .any(|t| t.user_id == uid && t.is_quick_pay)
    });
    assert!(!any_qp);

    // 5. PLEX vault transaction log & statistics endpoints
    router.db.with_mut_snapshot(|s| {
        record_plex_transaction(s, 701, uid, 500, 500, "Deposit", now);
        record_plex_transaction(s, 702, uid, -120, 380, "NES Purchase", now + 5);
    });
    assert_eq!(call(&router, &player, "eve_public.plex.vault.transaction.api.GetAllLoggedForUserRequest", Vec::new(), now).status_code, STATUS_OK);
    assert_eq!(call(&router, &player, "eve_public.plex.vault.transaction.api.GetLogRequest", Vec::new(), now).status_code, STATUS_BAD_REQUEST);
    let mut tx_missing = Vec::new();
    encode_varint_field(1, 999_999, &mut tx_missing);
    assert_eq!(call(&router, &player, "eve_public.plex.vault.transaction.api.GetLogRequest", tx_missing, now).status_code, STATUS_NOT_FOUND);
    let mut tx_found = Vec::new();
    encode_varint_field(1, 702, &mut tx_found);
    assert_eq!(call(&router, &player, "eve_public.plex.vault.transaction.api.GetLogRequest", tx_found, now).status_code, STATUS_OK);
    assert_eq!(call(&router, &player, "eve_public.plex.vault.transaction.api.GetStatisticsRequest", Vec::new(), now).status_code, STATUS_OK);
}
