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
use eve_proto::pb;
use eve_proto::quasar::{
    EvePublicRequest, EvePublicResponse, GoogleAny, GoogleTimestamp, QuasarDescriptorRegistry,
    WireField, WireMessage, WireValue, TYPE_GOOGLEAPIS_PREFIX,
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
    let curr_bytes = pb::message("eve.plex.Currency")
        .set("total_in_cents", cents)
        .must_encode();
    let msg = WireMessage {
        fields: vec![WireField {
            field_number: field,
            value: WireValue::LengthDelimited(curr_bytes),
        }],
    };
    out.extend_from_slice(&msg.encode());
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
    let bad_tx = pb::message("eve_public.plex.vault.transaction.api.GetLogRequest")
        .set("identifier.sequential", 999_999i64)
        .must_encode();
    assert_eq!(
        call(&router, &player, "eve_public.plex.vault.transaction.api.GetLogRequest", bad_tx, now + 6).status_code,
        STATUS_NOT_FOUND
    );
    let ok_tx = pb::message("eve_public.plex.vault.transaction.api.GetLogRequest")
        .set("identifier.sequential", tx_id as i64)
        .must_encode();
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
    let cat_99 = pb::message("eve_public.store.category.api.GetRequest")
        .set("category.sequential", 99u32)
        .must_encode();
    assert_eq!(call(&router, &player, "eve_public.store.category.api.GetRequest", cat_99, now).status_code, STATUS_NOT_FOUND);
    let cat_1 = pb::message("eve_public.store.category.api.GetRequest")
        .set("category.sequential", 1u32)
        .must_encode();
    assert_eq!(call(&router, &player, "eve_public.store.category.api.GetRequest", cat_1, now).status_code, STATUS_OK);

    assert_eq!(call(&router, &player, "eve_public.store.product.api.GetRequest", Vec::new(), now).status_code, STATUS_BAD_REQUEST);
    let prod_99 = pb::message("eve_public.store.product.api.GetRequest")
        .set("product.sequential", 99u32)
        .must_encode();
    assert_eq!(call(&router, &player, "eve_public.store.product.api.GetRequest", prod_99, now).status_code, STATUS_NOT_FOUND);
    let prod_102 = pb::message("eve_public.store.product.api.GetRequest")
        .set("product.sequential", 102u32)
        .must_encode();
    assert_eq!(call(&router, &player, "eve_public.store.product.api.GetRequest", prod_102, now).status_code, STATUS_OK);

    assert_eq!(call(&router, &player, "eve_public.store.offer.api.GetAllRequest", Vec::new(), now).status_code, STATUS_OK);
    assert_eq!(call(&router, &player, "eve_public.store.offer.api.GetRequest", Vec::new(), now).status_code, STATUS_BAD_REQUEST);
    let off_9999 = pb::message("eve_public.store.offer.api.GetRequest")
        .set("offer.store_offer", "9999")
        .must_encode();
    assert_eq!(call(&router, &player, "eve_public.store.offer.api.GetRequest", off_9999, now).status_code, STATUS_NOT_FOUND);
    let off_1002 = pb::message("eve_public.store.offer.api.GetRequest")
        .set("offer.store_offer", "1002")
        .must_encode();
    assert_eq!(call(&router, &player, "eve_public.store.offer.api.GetRequest", off_1002, now).status_code, STATUS_OK);

    assert_eq!(call(&router, &player, "eve_public.store.offer.api.GetAllTargetedRequest", Vec::new(), now).status_code, STATUS_OK);

    // Targeted offer pagination (page_size=1, offset="0" -> next_page="1")
    let page_req = pb::message("eve_public.store.targetaudience.offer.api.ListRequest")
        .set("page.size", 1u32)
        .set("page.token", "0")
        .must_encode();
    let rsp_ta_list = call(&router, &player, "eve_public.store.targetaudience.offer.api.ListRequest", page_req, now);
    assert_eq!(rsp_ta_list.status_code, STATUS_OK);

    // Targeted GetRequest: missing -> 400, unknown -> 404, unpromoted targeted -> 404, promoted -> 200
    assert_eq!(call(&router, &player, "eve_public.store.targetaudience.offer.api.GetRequest", Vec::new(), now).status_code, STATUS_BAD_REQUEST);
    let ta_req_bad = pb::message("eve_public.store.targetaudience.offer.api.GetRequest")
        .set("offer_id.catalogue_entry.name", "9999")
        .must_encode();
    assert_eq!(call(&router, &player, "eve_public.store.targetaudience.offer.api.GetRequest", ta_req_bad, now).status_code, STATUS_NOT_FOUND);

    let ta_req_ok = pb::message("eve_public.store.targetaudience.offer.api.GetRequest")
        .set("offer_id.catalogue_entry.name", "1003")
        .must_encode();
    assert_eq!(
        call(&router, &player, "eve_public.store.targetaudience.offer.api.GetRequest", ta_req_ok.clone(), now).status_code,
        STATUS_NOT_FOUND
    );
    router.db.with_mut_snapshot(|snap| {
        snap.quasar_commerce.targeted_offers.insert(
            "10:1003".into(),
            eve_db::TargetedOfferAdminRecord {
                user_id: 10,
                catalogue_entry_name: "1003".into(),
                promoted: true,
                retracted: false,
                earliest_reissue_unix: 0,
                partner_code: String::new(),
            },
        );
    });
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
    let bad_qty = pb::message("eve.fulfillment.api.DeliverItemRequest")
        .set("quantity", 0i32)
        .must_encode();
    assert_eq!(call(&router, &gm, "eve.fulfillment.api.DeliverItemRequest", bad_qty, now).status_code, STATUS_BAD_REQUEST);

    let no_station_req = pb::message("eve.fulfillment.api.DeliverItemRequest")
        .set("user.sequential", 90_000_099i32)
        .must_encode();
    assert_eq!(call(&router, &gm, "eve.fulfillment.api.DeliverItemRequest", no_station_req, now).status_code, STATUS_BAD_REQUEST);

    let deliver_req = pb::message("eve.fulfillment.api.DeliverItemRequest")
        .set("user.sequential", cid as i32)
        .set("item_type.sequential", 34599u64)
        .set("quantity", 3i32)
        .must_encode();
    assert_eq!(call(&router, &gm, "eve.fulfillment.api.DeliverItemRequest", deliver_req.clone(), now).status_code, STATUS_OK);
    // Second delivery stacks onto the existing hangar stack (3 + 3 = 6).
    assert_eq!(call(&router, &gm, "eve.fulfillment.api.DeliverItemRequest", deliver_req, now).status_code, STATUS_OK);
    let delivered_qty: i32 = router.db.with_snapshot(|s| {
        s.items.values().filter(|i| i.owner_id == cid && i.type_id == 34599).map(|i| i.quantity).sum()
    });
    assert_eq!(delivered_qty, 6);

    // 2. CostRequest: missing -> 400, unknown -> 404, no price_cents (1003) -> 501, valid (1001) -> 200
    assert_eq!(call(&router, &player, "eve_public.payment.purchase.api.CostRequest", Vec::new(), now).status_code, STATUS_BAD_REQUEST);
    let cost_9999 = pb::message("eve_public.payment.purchase.api.CostRequest")
        .set("catalog_amount_in_cents", 9999u32)
        .must_encode();
    assert_eq!(call(&router, &player, "eve_public.payment.purchase.api.CostRequest", cost_9999, now).status_code, STATUS_NOT_FOUND);
    let cost_1003 = pb::message("eve_public.payment.purchase.api.CostRequest")
        .set("catalog_amount_in_cents", 1003u32)
        .must_encode();
    assert_eq!(call(&router, &player, "eve_public.payment.purchase.api.CostRequest", cost_1003, now).status_code, STATUS_NOT_IMPLEMENTED);
    let cost_1001 = pb::message("eve_public.payment.purchase.api.CostRequest")
        .set("catalog_amount_in_cents", 1001u32)
        .must_encode();
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
    let tok_get_999 = pb::message("eve_public.payment.token.api.GetRequest")
        .set("token.sequential", 999u32)
        .must_encode();
    assert_eq!(call(&router, &player, "eve_public.payment.token.api.GetRequest", tok_get_999, now).status_code, STATUS_NOT_FOUND);
    let tok_get_501 = pb::message("eve_public.payment.token.api.GetRequest")
        .set("token.sequential", 501u32)
        .must_encode();
    assert_eq!(call(&router, &player, "eve_public.payment.token.api.GetRequest", tok_get_501, now).status_code, STATUS_OK);

    let encode_tok_purchase = |off: &str, tid: u64| -> Vec<u8> {
        pb::message("eve_public.payment.purchase.api.TokenRequest")
            .set("order.offer.store_offer", off)
            .set("order.quantity", 1u32)
            .set("token.sequential", tid as u32)
            .must_encode()
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
    let tok_req_503 = pb::message("eve_public.payment.purchase.api.TokenRequest")
        .set("order.offer.store_offer", "1002")
        .set("order.quantity", 2u32)
        .set("token.sequential", 503u32)
        .must_encode();
    assert_eq!(call(&router, &player, "eve_public.payment.purchase.api.TokenRequest", tok_req_503, now).status_code, STATUS_OK);

    let after_purchase_qty: i32 = router.db.with_snapshot(|s| {
        assert_eq!(s.characters[&cid].clone_grade, 2);
        s.items.values().filter(|i| i.owner_id == cid && i.type_id == 34599).map(|i| i.quantity).sum()
    });
    assert_eq!(after_purchase_qty, 10);

    // 4. Schedule GetActiveByTypeRequest & GetRequest + DisableAllRequest
    let active_sched_req = pb::message("eve_public.payment.schedule.api.GetActiveByTypeRequest")
        .set("type", 1i32)
        .must_encode();
    assert_eq!(call(&router, &player, "eve_public.payment.schedule.api.GetActiveByTypeRequest", active_sched_req.clone(), now).status_code, STATUS_OK);
    assert_eq!(call(&router, &other_player, "eve_public.payment.schedule.api.GetActiveByTypeRequest", active_sched_req, now).status_code, STATUS_NOT_FOUND);

    let sched_id = router.db.with_snapshot(|s| s.quasar_commerce.payment_schedules[0].id);
    assert_eq!(call(&router, &player, "eve_public.payment.schedule.api.GetRequest", Vec::new(), now).status_code, STATUS_BAD_REQUEST);
    let sched_req = pb::message("eve_public.payment.schedule.api.GetRequest")
        .set("schedule.sequential", sched_id)
        .must_encode();
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
    let tx_missing = pb::message("eve_public.plex.vault.transaction.api.GetLogRequest")
        .set("identifier.sequential", 999_999i64)
        .must_encode();
    assert_eq!(call(&router, &player, "eve_public.plex.vault.transaction.api.GetLogRequest", tx_missing, now).status_code, STATUS_NOT_FOUND);
    let tx_found = pb::message("eve_public.plex.vault.transaction.api.GetLogRequest")
        .set("identifier.sequential", 702i64)
        .must_encode();
    assert_eq!(call(&router, &player, "eve_public.plex.vault.transaction.api.GetLogRequest", tx_found, now).status_code, STATUS_OK);
    assert_eq!(call(&router, &player, "eve_public.plex.vault.transaction.api.GetStatisticsRequest", Vec::new(), now).status_code, STATUS_OK);
}

#[test]
fn sde_hydration_boundary_21_empty_names_and_plex_wire_payload_assertions() {
    use crate::quasar_gateway::NoticeScope;
    use eve_proto::quasar::{decode_zigzag64, WireMessage};
    use eve_sde::{InvType, SkinLicenseInfo};

    // 1. SDE hydration exact `> 20` boundary (`21` passes while `20` does not) + `.take(24)` + empty type_name fallback
    let mut sde_21 = (*shared_sde()).clone();
    sde_21.skin_licenses.clear();
    // Insert a custom type with empty name to trigger `.filter(|n| !n.is_empty()).unwrap_or_else(...)`
    sde_21.types.insert(
        34_599,
        InvType {
            type_id: 34_599,
            group_id: 1,
            category_id: 1,
            name: String::new(),
            mass: 1.0,
            volume: 1.0,
            capacity: 0.0,
            radius: 1.0,
            base_price: 0.0,
            published: true,
            portion_size: 1,
            market_group_id: None,
            race_id: None,
            faction_id: None,
            variation_parent_type_id: None,
            description_id: None,
            icon_id: None,
            graphic_id: None,
            sound_id: None,
            meta_group_id: None,
            meta_level: None,
            tech_level: None,
            default_effect_id: None,
            description: String::new(),
            effect_ids: Vec::new(),
            default_attributes: HashMap::new(),
        },
    );
    let mut sample_tids: Vec<u32> = sde_21
        .types
        .keys()
        .copied()
        .filter(|&tid| tid != 34_599)
        .take(25)
        .collect();
    sample_tids.sort_unstable();
    for &tid in &sample_tids[..21] {
        sde_21.skin_licenses.insert(
            tid,
            SkinLicenseInfo {
                license_type_id: tid,
                skin_id: tid,
                duration: -1,
            },
        );
    }
    let db_21 = WorldRepository::new_empty();
    db_21.with_mut_snapshot(|s| ensure_store_catalog_seeded(s, &sde_21));
    db_21.with_snapshot(|snap| {
        let cat = snap
            .quasar_commerce
            .nes_store_catalog
            .as_ref()
            .expect("21 skin licenses (> 20) must hydrate");
        assert_eq!(cat.offers.len(), 26); // 5 base + 21 skin offers
        assert_eq!(cat.products.len(), 26);
        // Empty InvType name falls back cleanly for 34_599
        let p102 = cat.products.iter().find(|p| p.id == 102).unwrap();
        assert_eq!(p102.name, "Multiple Pilot Training Certificate");
        assert_eq!(p102.description, "Multiple Pilot Training Certificate");
        // Verify skin formula: oid = 2001 + idx, pid = 200 + idx, price_plex = 55 + (tid % 4) * 55, targeted = idx % 4 == 0
        for (idx, &tid) in sample_tids[..21].iter().enumerate() {
            let off = cat.offers.iter().find(|o| o.id == 2001 + idx as u32).unwrap();
            assert_eq!(off.category_id, 2);
            assert_eq!(off.product_ids, vec![200 + idx as u32]);
            assert_eq!(off.price_plex, 55 + i64::from(tid % 4) * 55);
            assert_eq!(off.targeted, idx % 4 == 0);
        }
    });

    // `parse_raw_varints`: exact `idx + len == raw.len()` fits, whereas `len == remaining + 1` is ignored
    let inner_ok = WireMessage {
        fields: vec![WireField {
            field_number: 5,
            value: WireValue::Varint(77),
        }],
    }
    .encode();
    let exact_fit = WireMessage {
        fields: vec![WireField {
            field_number: 3,
            value: WireValue::LengthDelimited(inner_ok),
        }],
    }
    .encode();
    let parsed_fit = parse_raw_varints(&exact_fit);
    assert_eq!(parsed_fit.get(&3), Some(&77));
    assert_eq!(parsed_fit.get(&5), Some(&77));

    let mut one_byte_over = exact_fit.clone();
    one_byte_over[1] += 1;
    assert!(parse_raw_varints(&one_byte_over).is_empty());

    // 2. Balance / Deposit / Withdraw exact wire values, 99 vs 100 cent boundary, and BalanceChangedNotice payload
    let router = new_router();
    let uid = 71u64;
    let other_uid = 72u64;
    let gm = make_sess(Some(uid), Some(90_000_001), service_roles::ROLE_GM_DEFAULT);
    let player = make_sess(Some(uid), Some(90_000_001), service_roles::ROLE_PLAYER_DEFAULT);
    let other_player = make_sess(Some(other_uid), None, service_roles::ROLE_PLAYER_DEFAULT);
    let now = 1_760_000_000u64;

    // 99 cents truncates to 0 PLEX -> rejected (400); 100 cents == 1 PLEX -> accepted (200)
    let mut req_99 = Vec::new();
    encode_currency_field(3, 99, &mut req_99);
    assert_eq!(
        call(&router, &gm, "eve.plex.vault.assetholding.operation.api.DepositRequest", req_99, now).status_code,
        STATUS_BAD_REQUEST
    );
    let _ = router.pending_notices.write().unwrap().drain(..).count();
    router.routing_cache.write().unwrap().entries.clear();

    let mut req_100 = Vec::new();
    encode_currency_field(3, 100, &mut req_100);
    assert_eq!(
        call(&router, &gm, "eve.plex.vault.assetholding.operation.api.DepositRequest", req_100, now).status_code,
        STATUS_OK
    );

    let dep_notices: Vec<_> = router.routing_cache.write().unwrap().entries.drain(..).collect();
    assert_eq!(dep_notices.len(), 1);
    assert_eq!(dep_notices[0].scope, NoticeScope::User(uid as i32));
    let dep_payload = dep_notices[0].notice.payload.as_ref().unwrap();
    assert!(dep_payload.type_url.ends_with("BalanceChangedNotice"));
    let notice_wm = WireMessage::decode(&dep_payload.value).unwrap();
    let dep_attr_bytes = notice_wm.fields.iter().find(|f| f.field_number == 2).and_then(|f| f.value.as_bytes()).unwrap();
    let dep_attr_wm = WireMessage::decode(dep_attr_bytes).unwrap();
    // V24.01 `eve_public.plex.vault.transaction.Attributes` (#893): field 1 = timestamp, field 2 = amount_transferred, field 3 = resulting_balance
    assert!(
        dep_attr_wm.fields.iter().all(|f| matches!(f.field_number, 1..=3)),
        "#893: BalanceChangedNotice.attributes must only encode fields 1..=3, got {:?}",
        dep_attr_wm.fields
    );
    let notice_amt_bytes = dep_attr_wm.fields.iter().find(|f| f.field_number == 2).and_then(|f| f.value.as_bytes()).unwrap();
    let notice_amt_wm = WireMessage::decode(notice_amt_bytes).unwrap();
    assert_eq!(decode_zigzag64(notice_amt_wm.fields[0].value.as_varint().unwrap()), 100); // +1 PLEX * 100 cents
    let notice_bal_bytes = dep_attr_wm.fields.iter().find(|f| f.field_number == 3).and_then(|f| f.value.as_bytes()).unwrap();
    let notice_bal_wm = WireMessage::decode(notice_bal_bytes).unwrap();
    assert_eq!(decode_zigzag64(notice_bal_wm.fields[0].value.as_varint().unwrap()), 100);

    // Seed negative balance on other_uid to verify BalanceResponse zigzag64 negative encoding
    router.db.with_mut_snapshot(|snap| {
        snap.account_plex_vault.insert(other_uid, -50);
    });
    let neg_bal_rsp = call(&router, &other_player, "eve_public.plex.vault.api.BalanceRequest", Vec::new(), now);
    assert_eq!(neg_bal_rsp.status_code, STATUS_OK);
    let bal_outer = WireMessage::decode(&neg_bal_rsp.payload.unwrap().value).unwrap();
    let bal_curr = WireMessage::decode(bal_outer.fields[0].value.as_bytes().unwrap()).unwrap();
    assert_eq!(decode_zigzag64(bal_curr.fields[0].value.as_varint().unwrap()), -5_000);

    // 3. Transaction log & statistics exact wire decoding & cross-user isolation & zero/positive/negative classification
    router.db.with_mut_snapshot(|snap| {
        snap.quasar_commerce.plex_tx_logs.clear();
        record_plex_transaction(snap, 901, uid, 250, 251, "TopUp", now + 20);
        record_plex_transaction(snap, 902, uid, 0, 251, "ZeroAdjustment", now + 10);
        record_plex_transaction(snap, 903, uid, -80, 171, "SkinBuy", now + 30);
        record_plex_transaction(snap, 904, other_uid, 999, 949, "OtherUser", now + 1);
    });

    let all_tx_rsp = call(&router, &player, "eve_public.plex.vault.transaction.api.GetAllLoggedForUserRequest", Vec::new(), now);
    assert_eq!(all_tx_rsp.status_code, STATUS_OK);
    let all_tx_wm = WireMessage::decode(&all_tx_rsp.payload.unwrap().value).unwrap();
    let logged_ids: Vec<u64> = all_tx_wm
        .fields
        .iter()
        .map(|f| WireMessage::decode(f.value.as_bytes().unwrap()).unwrap().fields[0].value.as_varint().unwrap())
        .collect();
    assert_eq!(logged_ids, vec![901, 902, 903]); // excludes other_uid's 904!

    // Foreign user cannot read uid's transaction 903
    let tx_903 = pb::message("eve_public.plex.vault.transaction.api.GetLogRequest")
        .set("identifier.sequential", 903i64)
        .must_encode();
    assert_eq!(
        call(&router, &other_player, "eve_public.plex.vault.transaction.api.GetLogRequest", tx_903.clone(), now).status_code,
        STATUS_NOT_FOUND
    );
    let log_rsp = call(&router, &player, "eve_public.plex.vault.transaction.api.GetLogRequest", tx_903, now);
    assert_eq!(log_rsp.status_code, STATUS_OK);
    let log_attr = WireMessage::decode(WireMessage::decode(&log_rsp.payload.unwrap().value).unwrap().fields[0].value.as_bytes().unwrap()).unwrap();
    let log_amt_wm = WireMessage::decode(log_attr.fields.iter().find(|f| f.field_number == 2).unwrap().value.as_bytes().unwrap()).unwrap();
    let log_bal_wm = WireMessage::decode(log_attr.fields.iter().find(|f| f.field_number == 3).unwrap().value.as_bytes().unwrap()).unwrap();
    assert_eq!(decode_zigzag64(log_amt_wm.fields[0].value.as_varint().unwrap()), -8_000);
    assert_eq!(decode_zigzag64(log_bal_wm.fields[0].value.as_varint().unwrap()), 17_100);

    let stats_rsp = call(&router, &player, "eve_public.plex.vault.transaction.api.GetStatisticsRequest", Vec::new(), now);
    assert_eq!(stats_rsp.status_code, STATUS_OK);
    let stats_wm = WireMessage::decode(&stats_rsp.payload.unwrap().value).unwrap();
    let entry_wm = WireMessage::decode(stats_wm.fields.iter().find(|f| f.field_number == 1).unwrap().value.as_bytes().unwrap()).unwrap();
    let inc_wm = WireMessage::decode(entry_wm.fields.iter().find(|f| f.field_number == 3).unwrap().value.as_bytes().unwrap()).unwrap();
    let exp_wm = WireMessage::decode(entry_wm.fields.iter().find(|f| f.field_number == 4).unwrap().value.as_bytes().unwrap()).unwrap();
    let cnt = entry_wm.fields.iter().find(|f| f.field_number == 5).unwrap().value.as_varint().unwrap();
    let earliest_wm = WireMessage::decode(stats_wm.fields.iter().find(|f| f.field_number == 2).unwrap().value.as_bytes().unwrap()).unwrap();
    assert_eq!(decode_zigzag64(inc_wm.fields[0].value.as_varint().unwrap()), 25_000);
    assert_eq!(decode_zigzag64(exp_wm.fields[0].value.as_varint().unwrap()), 8_000);
    assert_eq!(cnt, 3);
    assert_eq!(earliest_wm.fields[0].value.as_varint().unwrap(), now + 10);
}

#[test]
fn store_catalog_wire_fields_schedule_token_isolation_and_omega_purchase_edges() {
    use crate::quasar_gateway::NoticeScope;
    use eve_proto::quasar::WireMessage;

    let router = new_router();
    seed_catalog(&router.db);
    // Add a non-1001 category-1 Omega+item offer (1099) & zero-type product (105) to test category_id==1 + product filtering
    router.db.with_mut_snapshot(|snap| {
        let mut cat = read_store_catalog(snap);
        cat.products.push(StoreProduct {
            id: 199,
            name: "VirtualToken".into(),
            description: "No type_id".into(),
            type_id: 0,
            quantity: 1,
        });
        cat.offers.push(StoreOffer {
            id: 1099,
            name: "Omega+SKIN Bundle".into(),
            description: "Category 1 + Item".into(),
            category_id: 1,
            price_plex: 600,
            product_ids: vec![102, 199],
            targeted: false,
            price_cents: Some(2_500),
            currency: Some("GBP".into()),
        });
        save_store_catalog(snap, &cat);
    });

    let u1 = 81u64;
    let u2 = 82u64;
    let cid1 = 90_000_081i64;
    let structure_id = 1_000_000_000_777i64;
    router.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().unwrap().clone();
        ch.character_id = cid1;
        ch.user_id = u1;
        ch.station_id = None;
        ch.structure_id = Some(structure_id);
        ch.home_station_id = 60_003_760;
        snap.characters.insert(cid1, ch);
        // Existing singleton item of type 34599 in structure hangar must NOT be stacked onto!
        let sing_id = 990_000_777i64;
        snap.items.insert(
            sing_id,
            eve_db::InventoryItemRecord {
                item_id: sing_id,
                type_id: 34_599,
                owner_id: cid1,
                location_id: structure_id,
                flag: eve_db::InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });

    let p1 = make_sess(Some(u1), Some(cid1), service_roles::ROLE_PLAYER_DEFAULT);
    let gm1 = make_sess(Some(u1), Some(cid1), service_roles::ROLE_GM_DEFAULT);
    let p2_no_char = make_sess(Some(u2), None, service_roles::ROLE_PLAYER_DEFAULT);
    let now = 1_760_000_500u64;

    // 1. Store wire decoding: GetAll vs GetAllTargeted strict disjoint filter + Get offer tags & price_plex * 100
    let off_all = WireMessage::decode(&call(&router, &p1, "eve_public.store.offer.api.GetAllRequest", Vec::new(), now).payload.unwrap().value).unwrap();
    let pub_ids: Vec<String> = off_all
        .fields
        .iter()
        .map(|f| WireMessage::decode(f.value.as_bytes().unwrap()).unwrap().fields[0].value.as_str().unwrap().unwrap().to_string())
        .collect();
    assert_eq!(pub_ids, vec!["1001", "1002", "1099"]);

    let off_tgt = WireMessage::decode(&call(&router, &p1, "eve_public.store.offer.api.GetAllTargetedRequest", Vec::new(), now).payload.unwrap().value).unwrap();
    let tgt_ids: Vec<String> = off_tgt
        .fields
        .iter()
        .map(|f| WireMessage::decode(f.value.as_bytes().unwrap()).unwrap().fields[0].value.as_str().unwrap().unwrap().to_string())
        .collect();
    assert_eq!(tgt_ids, vec!["1003", "1004"]);

    let get_1002 = pb::message("eve_public.store.offer.api.GetRequest")
        .set("offer.store_offer", "1002")
        .must_encode();
    let off_1002_wm = WireMessage::decode(&call(&router, &p1, "eve_public.store.offer.api.GetRequest", get_1002, now).payload.unwrap().value).unwrap();
    let off_1002_attr = WireMessage::decode(off_1002_wm.fields[0].value.as_bytes().unwrap()).unwrap();
    let tags: Vec<&str> = off_1002_attr.fields.iter().filter(|f| f.field_number == 2).filter_map(|f| f.value.as_str().ok().flatten()).collect();
    assert_eq!(tags, vec!["featured", "hot"]);
    let price_wm = WireMessage::decode(off_1002_attr.fields.iter().find(|f| f.field_number == 12).unwrap().value.as_bytes().unwrap()).unwrap();
    let plex_price_wm = WireMessage::decode(price_wm.fields.iter().find(|f| f.field_number == 6).unwrap().value.as_bytes().unwrap()).unwrap();
    assert_eq!(
        eve_proto::quasar::decode_zigzag64(plex_price_wm.fields[0].value.as_varint().unwrap()),
        11_000
    ); // 110 PLEX * 100

    // CostRequest decoded fields for offer 1002 (499 cents EUR)
    let cost_1002 = pb::message("eve_public.payment.purchase.api.CostRequest")
        .set("catalog_amount_in_cents", 1002u32)
        .must_encode();
    let cost_wm = WireMessage::decode(&call(&router, &p1, "eve_public.payment.purchase.api.CostRequest", cost_1002, now).payload.unwrap().value).unwrap();
    let cost_inner = WireMessage::decode(cost_wm.fields[0].value.as_bytes().unwrap()).unwrap();
    assert_eq!(cost_inner.fields.iter().find(|f| f.field_number == 1).unwrap().value.as_varint(), Some(499));
    assert_eq!(cost_inner.fields.iter().find(|f| f.field_number == 2).unwrap().value.as_varint(), Some(0));
    assert_eq!(cost_inner.fields.iter().find(|f| f.field_number == 3).unwrap().value.as_varint(), Some(499));
    assert_eq!(cost_inner.fields.iter().find(|f| f.field_number == 5).unwrap().value.as_str().unwrap(), Some("EUR"));

    // 2. Token isolation (u1 vs u2) & DisableAll only touches caller's user_id & emits QuickPayRemovedNotice
    add_user_quick_pay_token(&router.db, &router.pending_notices, u1, PaymentTokenRecord { id: 801, user_id: u1, token_str: "u1-qp".into(), is_quick_pay: true, consumed: false });
    add_user_quick_pay_token(&router.db, &router.pending_notices, u1, PaymentTokenRecord { id: 802, user_id: u1, token_str: "u1-noqp".into(), is_quick_pay: false, consumed: false });
    add_user_quick_pay_token(&router.db, &router.pending_notices, u2, PaymentTokenRecord { id: 803, user_id: u2, token_str: "u2-qp".into(), is_quick_pay: true, consumed: false });
    let _ = router.pending_notices.write().unwrap().drain(..).count();

    let qp_u1 = WireMessage::decode(&call(&router, &p1, "eve_public.payment.token.api.GetQuickPayRequest", Vec::new(), now).payload.unwrap().value).unwrap();
    let qp_ids: Vec<u64> = qp_u1.fields.iter().map(|f| WireMessage::decode(f.value.as_bytes().unwrap()).unwrap().fields[0].value.as_varint().unwrap()).collect();
    assert_eq!(qp_ids, vec![801]);

    // DisableAllRequest for u1 must NOT disable u2's token 803!
    let _ = router.pending_notices.write().unwrap().drain(..).count();
    router.routing_cache.write().unwrap().entries.clear();
    assert_eq!(call(&router, &p1, "eve_public.payment.token.api.DisableAllRequest", Vec::new(), now).status_code, STATUS_OK);
    let dis_notices: Vec<_> = router.routing_cache.write().unwrap().entries.drain(..).collect();
    assert_eq!(dis_notices.len(), 1);
    assert_eq!(dis_notices[0].scope, NoticeScope::User(u1 as i32));
    assert!(dis_notices[0].notice.payload.as_ref().unwrap().type_url.ends_with("QuickPayRemovedNotice"));
    router.db.with_snapshot(|snap| {
        assert!(!snap.quasar_commerce.payment_tokens.iter().find(|t| t.id == 801).unwrap().is_quick_pay);
        assert!(snap.quasar_commerce.payment_tokens.iter().find(|t| t.id == 803).unwrap().is_quick_pay);
    });

    // 3. DeliverItemRequest defaults (type_id=44992, qty=1), structure_id fallback, singleton isolation, version += 1
    assert_eq!(call(&router, &gm1, "eve.fulfillment.api.DeliverItemRequest", Vec::new(), now).status_code, STATUS_OK);
    router.db.with_snapshot(|snap| {
        let plex_stacks: Vec<_> = snap.items.values().filter(|i| i.owner_id == cid1 && i.type_id == 44_992 && i.location_id == structure_id).collect();
        assert_eq!(plex_stacks.len(), 1);
        assert_eq!(plex_stacks[0].quantity, 1);
    });

    // 4. TokenRequest:
    // - u2 has ZERO characters & no station, yet offer_id == 1001 succeeds!
    // - qty=0 clamps to 1, qty=20_000 clamps to 10_000; category_id==1 offer 1099 activates Omega + delivers non-singleton stack to structure_id!
    add_user_quick_pay_token(&router.db, &router.pending_notices, u1, PaymentTokenRecord { id: 804, user_id: u1, token_str: "u1-omega".into(), is_quick_pay: true, consumed: false });
    add_user_quick_pay_token(&router.db, &router.pending_notices, u2, PaymentTokenRecord { id: 805, user_id: u2, token_str: "u2-nochar".into(), is_quick_pay: true, consumed: false });
    let _ = router.pending_notices.write().unwrap().drain(..).count();

    let encode_purchase = |off: &str, tok: u64, qty: u64| -> Vec<u8> {
        pb::message("eve_public.payment.purchase.api.TokenRequest")
            .set("order.offer.store_offer", off)
            .set("order.quantity", qty as u32)
            .set("token.sequential", tok as u32)
            .must_encode()
    };

    // Offer 1001 for u2 with qty=0 (clamped to 1 -> 1_999 cents, 30 days) even with no characters
    let rsp_u2 = call(&router, &p2_no_char, "eve_public.payment.purchase.api.TokenRequest", encode_purchase("1001", 805, 0), now);
    assert_eq!(rsp_u2.status_code, STATUS_OK);

    // Offer 1099 for u1 with qty=20_000 (clamped to 10_000 -> 2_500 * 10_000 cents, product 102 * 2, skips type_id=0 product 105)
    let rsp_u1 = call(&router, &p1, "eve_public.payment.purchase.api.TokenRequest", encode_purchase("1099", 804, 20_000), now);
    assert_eq!(rsp_u1.status_code, STATUS_OK);
    let rsp_u1_wm = WireMessage::decode(&rsp_u1.payload.unwrap().value).unwrap();
    let rec_wm = WireMessage::decode(rsp_u1_wm.fields[0].value.as_bytes().unwrap()).unwrap();
    assert_eq!(rec_wm.fields.iter().find(|f| f.field_number == 3).unwrap().value.as_varint(), Some(1));
    assert_eq!(rec_wm.fields.iter().find(|f| f.field_number == 4).unwrap().value.as_str().unwrap(), Some("Payment completed successfully"));

    router.db.with_snapshot(|snap| {
        let u2_rec = snap.quasar_commerce.payment_receipts.iter().find(|r| r.user_id == u2).unwrap();
        assert_eq!(u2_rec.amount_in_cents, 1_999);
        assert_eq!(u2_rec.offer_id, "1001");

        let u1_rec = snap.quasar_commerce.payment_receipts.iter().find(|r| r.user_id == u1).unwrap();
        assert_eq!(u1_rec.amount_in_cents, 2_500 * 10_000);
        assert_eq!(u1_rec.currency, "GBP");
        assert_eq!(snap.characters[&cid1].clone_grade, 2);
        // Zero-type product 105 never creates an item with type_id 0
        assert!(!snap.items.values().any(|i| i.owner_id == cid1 && i.type_id == 0));
        // Singleton 34599 stays untouched at qty 1 while delivered non-singleton stack holds 2 * 10_000 = 20_000 at structure_id
        let delivered: Vec<_> = snap.items.values().filter(|i| i.owner_id == cid1 && i.type_id == 34_599 && i.location_id == structure_id).collect();
        assert_eq!(delivered.len(), 2);
        assert_eq!(delivered.iter().find(|i| i.singleton).unwrap().quantity, 1);
        assert_eq!(delivered.iter().find(|i| !i.singleton).unwrap().quantity, 20_000);
        // Schedule uses VEVC-1099 and interval 30 * 10_000
        let sched = snap.quasar_commerce.payment_schedules.iter().find(|s| s.user_id == u1).unwrap();
        assert_eq!(sched.offer_id, "VEVC-1099");
        assert_eq!(sched.interval_days, 30 * 10_000);
    });
}

#[test]
fn v2402_omega_upsell_offer_and_product_proto_attributes() {
    use eve_proto::pb::enum_name;
    use eve_proto::quasar::{DynamicQuasarMessage, DynamicValue};

    let router = new_router();
    let reg = QuasarDescriptorRegistry::open_default_or_empty();
    let now = 1_700_000_900u64;
    let player = make_sess(Some(10), Some(90_000_010), 0);

    fn dyn_list(msg: &DynamicQuasarMessage, field: u32) -> Vec<&DynamicValue> {
        match msg.get(field) {
            Some(DynamicValue::Repeated(vs)) => vs.iter().collect(),
            Some(v) => vec![v],
            None => Vec::new(),
        }
    }
    fn as_msg(val: Option<&DynamicValue>) -> &DynamicQuasarMessage {
        match val {
            Some(DynamicValue::Message(m)) => m.as_ref(),
            other => panic!("expected submessage, got {other:?}"),
        }
    }

    // 1. Offer 1001 GetRequest populates V24.02 required attributes (pricing, products, categories, schedule, tags)
    let off_req = pb::message("eve_public.store.offer.api.GetRequest")
        .set("offer.store_offer", "1001")
        .must_encode();
    let off_rsp = call(
        &router,
        &player,
        "eve_public.store.offer.api.GetRequest",
        off_req,
        now,
    );
    assert_eq!(off_rsp.status_code, STATUS_OK);
    let off_any = off_rsp.payload.expect("offer GetResponse payload");
    let off_dec = reg
        .decode_dynamic(&off_any.type_url, &off_any.value)
        .expect("decode offer GetResponse");
    let attr = as_msg(off_dec.get(1));
    assert_eq!(
        attr.get(1),
        Some(&DynamicValue::String("30 Days Omega Clone State".into()))
    );
    let tags: Vec<&str> = dyn_list(attr, 2)
        .into_iter()
        .filter_map(|v| match v {
            DynamicValue::String(s) => Some(s.as_str()),
            _ => None,
        })
        .collect();
    assert!(tags.contains(&"UpsellStarterPack"));
    assert!(tags.contains(&"omega"));
    assert_eq!(attr.get(3), Some(&DynamicValue::Bool(false)));
    assert_eq!(attr.get(4), Some(&DynamicValue::Bool(false)));
    assert_eq!(attr.get(5), Some(&DynamicValue::Bool(true)));
    assert_eq!(attr.get(7), Some(&DynamicValue::Bool(true)));

    for price_fnum in [12u32, 13u32] {
        let price = as_msg(attr.get(price_fnum));
        let cur = as_msg(price.get(4));
        assert_eq!(cur.get(1), Some(&DynamicValue::String("USD".into())));
        assert_eq!(cur.get(2), Some(&DynamicValue::Int64(1_999)));
        let plex = as_msg(price.get(6));
        assert_eq!(plex.get(1), Some(&DynamicValue::Sint64(50_000)));
    }

    let sched = as_msg(attr.get(10));
    assert_eq!(
        as_msg(sched.get(1)).get(1),
        Some(&DynamicValue::Int64(2_592_000))
    );
    assert_eq!(
        as_msg(sched.get(2)).get(1),
        Some(&DynamicValue::String("1001".into()))
    );
    assert_eq!(
        as_msg(as_msg(sched.get(3)).get(4)).get(2),
        Some(&DynamicValue::Int64(1_999))
    );
    assert_eq!(
        as_msg(as_msg(sched.get(4)).get(4)).get(2),
        Some(&DynamicValue::Int64(1_999))
    );

    let cats = dyn_list(attr, 14);
    assert_eq!(cats.len(), 1);
    let c0 = as_msg(Some(cats[0]));
    assert_eq!(c0.get(1), Some(&DynamicValue::Uint32(1)));
    assert_eq!(c0.get(2), Some(&DynamicValue::Enum(2)));

    let prods = dyn_list(attr, 15);
    assert_eq!(prods.len(), 1);
    let p0 = as_msg(Some(prods[0]));
    assert_eq!(as_msg(p0.get(1)).get(1), Some(&DynamicValue::Uint32(101)));
    assert_eq!(as_msg(p0.get(1)).get(2), Some(&DynamicValue::Enum(2)));
    assert_eq!(p0.get(2), Some(&DynamicValue::Int32(1)));

    // 2. Product 101 emits attributes.license (duration = 30d, type = eve_clonestate_omega), not attributes.item
    let p101_req = pb::message("eve_public.store.product.api.GetRequest")
        .set("product.sequential", 101u32)
        .set("product.catalog", enum_name("CATALOG_REAL_MONEY"))
        .must_encode();
    let p101_rsp = call(
        &router,
        &player,
        "eve_public.store.product.api.GetRequest",
        p101_req,
        now,
    );
    assert_eq!(p101_rsp.status_code, STATUS_OK);
    let p101_any = p101_rsp.payload.expect("product 101 payload");
    let p101_dec = reg
        .decode_dynamic(&p101_any.type_url, &p101_any.value)
        .expect("decode product 101");
    let p101_attr = as_msg(p101_dec.get(1));
    assert!(p101_attr.get(2).is_none());
    let lic = as_msg(p101_attr.get(6));
    assert_eq!(
        as_msg(lic.get(1)).get(1),
        Some(&DynamicValue::Int64(2_592_000))
    );
    assert_eq!(
        lic.get(2),
        Some(&DynamicValue::String("eve_clonestate_omega".into()))
    );

    // 3. Item product 102 (type_id=34599) and SP claim product 105 (type_id=0) emit attributes.item, not license
    for (pid, expected_tid) in [(102u32, 34_599u64), (105u32, 0u64)] {
        let preq = pb::message("eve_public.store.product.api.GetRequest")
            .set("product.sequential", pid)
            .must_encode();
        let prsp = call(
            &router,
            &player,
            "eve_public.store.product.api.GetRequest",
            preq,
            now,
        );
        assert_eq!(prsp.status_code, STATUS_OK);
        let pany = prsp.payload.unwrap();
        let pdec = reg.decode_dynamic(&pany.type_url, &pany.value).unwrap();
        let pattr = as_msg(pdec.get(1));
        assert!(pattr.get(6).is_none());
        let item = as_msg(pattr.get(2));
        assert_eq!(
            as_msg(item.get(1)).get(1),
            Some(&DynamicValue::Uint64(expected_tid))
        );
    }

    // 4. GetAllRequest honors catalog filter + unknown offer/product return 404
    router.db.with_mut_snapshot(|snap| {
        let mut cat = read_store_catalog(snap);
        if !cat.offers.iter().any(|o| o.id == 9001) {
            cat.offers.push(StoreOffer {
                id: 9001,
                name: "PLEX SKIN Test".into(),
                description: "PLEX only".into(),
                category_id: 2,
                price_plex: 55,
                product_ids: vec![102],
                targeted: false,
                price_cents: None,
                currency: None,
            });
            save_store_catalog(snap, &cat);
        }
    });
    let all_rm = pb::message("eve_public.store.offer.api.GetAllRequest")
        .set("catalog", enum_name("CATALOG_REAL_MONEY"))
        .must_encode();
    let all_rm_rsp = call(
        &router,
        &player,
        "eve_public.store.offer.api.GetAllRequest",
        all_rm,
        now,
    );
    assert_eq!(all_rm_rsp.status_code, STATUS_OK);
    let all_rm_dec = reg
        .decode_dynamic(
            &all_rm_rsp.payload.as_ref().unwrap().type_url,
            &all_rm_rsp.payload.as_ref().unwrap().value,
        )
        .unwrap();
    let rm_ids: Vec<String> = dyn_list(&all_rm_dec, 1)
        .into_iter()
        .filter_map(|v| match as_msg(Some(v)).get(1) {
            Some(DynamicValue::String(s)) => Some(s.clone()),
            _ => None,
        })
        .collect();
    assert!(rm_ids.contains(&"1001".to_string()));
    assert!(!rm_ids.contains(&"1005".to_string()));
    assert!(!rm_ids.contains(&"9001".to_string()));

    let all_nes = pb::message("eve_public.store.offer.api.GetAllRequest")
        .set("catalog", enum_name("CATALOG_NES"))
        .must_encode();
    let all_nes_rsp = call(
        &router,
        &player,
        "eve_public.store.offer.api.GetAllRequest",
        all_nes,
        now,
    );
    assert_eq!(all_nes_rsp.status_code, STATUS_OK);
    let all_nes_dec = reg
        .decode_dynamic(
            &all_nes_rsp.payload.as_ref().unwrap().type_url,
            &all_nes_rsp.payload.as_ref().unwrap().value,
        )
        .unwrap();
    let nes_ids: Vec<String> = dyn_list(&all_nes_dec, 1)
        .into_iter()
        .filter_map(|v| match as_msg(Some(v)).get(1) {
            Some(DynamicValue::String(s)) => Some(s.clone()),
            _ => None,
        })
        .collect();
    assert!(nes_ids.contains(&"1001".to_string()));
    assert!(nes_ids.contains(&"1005".to_string()));
    assert!(nes_ids.contains(&"9001".to_string()));

    let miss_off = pb::message("eve_public.store.offer.api.GetRequest")
        .set("offer.store_offer", "999999")
        .must_encode();
    assert_eq!(
        call(
            &router,
            &player,
            "eve_public.store.offer.api.GetRequest",
            miss_off,
            now
        )
        .status_code,
        STATUS_NOT_FOUND
    );
    let miss_prod = pb::message("eve_public.store.product.api.GetRequest")
        .set("product.sequential", 999_999u32)
        .must_encode();
    assert_eq!(
        call(
            &router,
            &player,
            "eve_public.store.product.api.GetRequest",
            miss_prod,
            now
        )
        .status_code,
        STATUS_NOT_FOUND
    );
}

#[test]
fn targeted_offer_audience_and_non_1001_omega_delivery_parity() {
    use eve_db::service_domains::quasar_and_social::{
        PaymentTokenRecord, StoreCategory, StoreOffer, StoreProduct,
    };
    use eve_db::TargetedOfferAdminRecord;

    let now = 1_760_000_000_u64;
    let db = Arc::new(WorldRepository::with_demo_seed());
    let router = PublicGatewayRouter::new(
        QuasarDescriptorRegistry::open_default_or_empty(),
        Arc::clone(&db),
        shared_sde(),
    );

    let uid_ok = db.provision_player_account("ta_ok", "pw");
    let cid_ok = db
        .create_character(uid_ok, "TA Eligible", 1, 1, 11, 17, &shared_sde())
        .expect("create ok char")
        .character_id;
    let uid_ret = db.provision_player_account("ta_ret", "pw");
    let uid_wait = db.provision_player_account("ta_wait", "pw");
    let uid_nochar = db.provision_player_account("omega_virtual_only", "pw");

    db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&cid_ok) {
            ch.clone_grade = 1;
            ch.home_station_id = 60_003_760;
        }
        snap.account_plex_vault.insert(uid_ok, 500);
        snap.account_plex_vault.insert(uid_ret, 500);
        snap.account_plex_vault.insert(uid_wait, 500);
        let cat = StoreCatalog {
            categories: vec![
                StoreCategory { id: 1, name: "Omega".into(), description: "Omega".into() },
                StoreCategory { id: 2, name: "SKINs".into(), description: "SKINs".into() },
                StoreCategory { id: 3, name: "Bundles".into(), description: "Bundles".into() },
            ],
            products: vec![
                StoreProduct {
                    id: 201,
                    name: "30d Omega Pass".into(),
                    description: "Virtual Omega time".into(),
                    type_id: 0,
                    quantity: 1,
                },
                StoreProduct {
                    id: 202,
                    name: "Bundle SKIN".into(),
                    description: "Physical SKIN".into(),
                    type_id: 34_599,
                    quantity: 1,
                },
            ],
            offers: vec![
                StoreOffer {
                    id: 8001,
                    name: "Virtual Omega Bundle".into(),
                    description: "Non-1001 non-cat-1 virtual Omega".into(),
                    category_id: 3,
                    price_plex: 0,
                    product_ids: vec![201],
                    targeted: false,
                    price_cents: Some(1_499),
                    currency: Some("USD".into()),
                },
                StoreOffer {
                    id: 8002,
                    name: "Targeted Promo Pack".into(),
                    description: "User-restricted offer".into(),
                    category_id: 2,
                    price_plex: 100,
                    product_ids: vec![202],
                    targeted: true,
                    price_cents: Some(399),
                    currency: Some("USD".into()),
                },
                StoreOffer {
                    id: 8003,
                    name: "Combo Omega Physical Bundle".into(),
                    description: "Non-1001 non-cat-1 Omega + SKIN".into(),
                    category_id: 3,
                    price_plex: 0,
                    product_ids: vec![201, 202],
                    targeted: false,
                    price_cents: Some(1_999),
                    currency: Some("USD".into()),
                },
            ],
        };
        save_store_catalog(snap, &cat);
        snap.quasar_commerce.targeted_offers.insert(
            format!("{uid_ok}:8002"),
            TargetedOfferAdminRecord {
                user_id: uid_ok,
                catalogue_entry_name: "8002".into(),
                promoted: true,
                retracted: false,
                earliest_reissue_unix: 0,
                partner_code: String::new(),
            },
        );
        snap.quasar_commerce.targeted_offers.insert(
            format!("{uid_ret}:8002"),
            TargetedOfferAdminRecord {
                user_id: uid_ret,
                catalogue_entry_name: "8002".into(),
                promoted: true,
                retracted: true,
                earliest_reissue_unix: 0,
                partner_code: String::new(),
            },
        );
        snap.quasar_commerce.targeted_offers.insert(
            format!("{uid_wait}:8002"),
            TargetedOfferAdminRecord {
                user_id: uid_wait,
                catalogue_entry_name: "8002".into(),
                promoted: true,
                retracted: false,
                earliest_reissue_unix: (now + 3_600) as i64,
                partner_code: String::new(),
            },
        );
        snap.quasar_commerce.payment_tokens = vec![
            PaymentTokenRecord {
                id: 701,
                user_id: uid_ok,
                token_str: "tok_ok".into(),
                is_quick_pay: true,
                consumed: false,
            },
            PaymentTokenRecord {
                id: 702,
                user_id: uid_nochar,
                token_str: "tok_nochar".into(),
                is_quick_pay: true,
                consumed: false,
            },
        ];
    });

    let sess_ok = make_sess(Some(uid_ok), Some(cid_ok), service_roles::ROLE_PLAYER_DEFAULT);
    let sess_ret = make_sess(Some(uid_ret), None, service_roles::ROLE_PLAYER_DEFAULT);
    let sess_wait = make_sess(Some(uid_wait), None, service_roles::ROLE_PLAYER_DEFAULT);
    let sess_nochar = make_sess(Some(uid_nochar), None, service_roles::ROLE_PLAYER_DEFAULT);

    // 1. targetaudience.offer.api.ListRequest filters per-user eligibility
    let list_ok = call(&router, &sess_ok, "eve_public.store.targetaudience.offer.api.ListRequest", Vec::new(), now);
    assert_eq!(list_ok.status_code, STATUS_OK);
    assert!(
        String::from_utf8_lossy(&list_ok.payload.as_ref().unwrap().value).contains("8002"),
        "eligible user must receive targeted offer 8002 in ListResponse"
    );

    for denied_sess in [&sess_ret, &sess_wait] {
        let list_denied = call(
            &router,
            denied_sess,
            "eve_public.store.targetaudience.offer.api.ListRequest",
            Vec::new(),
            now,
        );
        assert_eq!(list_denied.status_code, STATUS_OK);
        assert!(
            !String::from_utf8_lossy(&list_denied.payload.as_ref().unwrap().value).contains("8002"),
            "retracted or future-reissue user must NOT receive targeted offer 8002"
        );
    }

    // 2. targetaudience.offer.api.GetRequest checks cross-user isolation & eligibility
    let get_req_ok = pb::message("eve_public.store.targetaudience.offer.api.GetRequest")
        .set("offer_id.user_id.sequential", uid_ok as i32)
        .set("offer_id.catalogue_entry.name", "8002")
        .must_encode();
    assert_eq!(
        call(&router, &sess_ok, "eve_public.store.targetaudience.offer.api.GetRequest", get_req_ok.clone(), now).status_code,
        STATUS_OK
    );
    assert_eq!(
        call(&router, &sess_ret, "eve_public.store.targetaudience.offer.api.GetRequest", get_req_ok, now).status_code,
        STATUS_FORBIDDEN
    );
    let get_req_ret = pb::message("eve_public.store.targetaudience.offer.api.GetRequest")
        .set("offer_id.user_id.sequential", uid_ret as i32)
        .set("offer_id.catalogue_entry.name", "8002")
        .must_encode();
    assert_eq!(
        call(&router, &sess_ret, "eve_public.store.targetaudience.offer.api.GetRequest", get_req_ret, now).status_code,
        STATUS_NOT_FOUND
    );
    let get_req_wait = pb::message("eve_public.store.targetaudience.offer.api.GetRequest")
        .set("offer_id.user_id.sequential", uid_wait as i32)
        .set("offer_id.catalogue_entry.name", "8002")
        .must_encode();
    assert_eq!(
        call(&router, &sess_wait, "eve_public.store.targetaudience.offer.api.GetRequest", get_req_wait, now).status_code,
        STATUS_NOT_FOUND
    );

    // 3. PurchaseRequest enforces targeted audience eligibility
    let purch_8002 = WireMessage {
        fields: vec![
            WireField { field_number: 1, value: WireValue::Varint(8002) },
            WireField { field_number: 2, value: WireValue::Varint(1) },
        ],
    }
    .encode();
    assert_eq!(
        call(&router, &sess_ret, "eve_public.plex.vault.api.PurchaseRequest", purch_8002.clone(), now).status_code,
        STATUS_FORBIDDEN
    );
    assert_eq!(
        call(&router, &sess_wait, "eve_public.plex.vault.api.PurchaseRequest", purch_8002.clone(), now).status_code,
        STATUS_FORBIDDEN
    );
    assert_eq!(
        call(&router, &sess_ok, "eve_public.plex.vault.api.PurchaseRequest", purch_8002, now).status_code,
        STATUS_OK
    );
    assert_eq!(db.with_snapshot(|s| s.account_plex_vault.get(&uid_ok).copied()), Some(400));

    // 4. TokenRequest activates non-1001 non-cat-1 virtual Omega offer 8001 without a character/station
    let tok_8001 = WireMessage {
        fields: vec![
            WireField { field_number: 1, value: WireValue::Varint(8001) },
            WireField { field_number: 2, value: WireValue::Varint(702) },
        ],
    }
    .encode();
    assert_eq!(
        call(&router, &sess_nochar, "eve_public.payment.purchase.api.TokenRequest", tok_8001, now).status_code,
        STATUS_OK
    );
    assert!(
        db.with_snapshot(|s| {
            s.owner_notes
                .values()
                .find(|n| n.label == "S:QUASAR_IDENTITY_STORE")
                .and_then(|n| {
                    serde_json::from_str::<crate::services::quasar_auth_user::QuasarIdentityStore>(&n.note)
                        .ok()
                })
                .and_then(|st| st.omega_subscriptions.get(&uid_nochar).copied())
                .unwrap_or(0)
                > now
        }),
        "virtual non-1001 Omega offer 8001 must activate Omega entitlement without station"
    );

    // 5. TokenRequest on non-1001 combo offer 8003 activates Omega AND delivers physical SKIN
    let tok_8003 = WireMessage {
        fields: vec![
            WireField { field_number: 1, value: WireValue::Varint(8003) },
            WireField { field_number: 2, value: WireValue::Varint(701) },
        ],
    }
    .encode();
    assert_eq!(
        call(&router, &sess_ok, "eve_public.payment.purchase.api.TokenRequest", tok_8003, now).status_code,
        STATUS_OK
    );
    db.with_snapshot(|s| {
        assert_eq!(s.characters.get(&cid_ok).map(|c| c.clone_grade), Some(2));
        let skin_qty: i32 = s
            .items
            .values()
            .filter(|it| it.owner_id == cid_ok && it.type_id == 34_599)
            .map(|it| it.quantity)
            .sum();
        assert!(skin_qty >= 2, "expected targeted 8002 SKIN + combo 8003 SKIN delivered");
    });
}

#[test]
fn targeted_fail_closed_string_offer_id_multi_month_omega_and_item_notifications() {
    let router = new_router();
    let db = Arc::clone(&router.db);
    let ctx = Arc::new(crate::router::ServerContext::with_sde_and_repository(
        shared_sde(),
        Arc::clone(&db),
    ));
    let uid = 42_001u64;
    let cid = 94_200_001i64;
    let station_id = 60_003_760i64;
    let now = 1_700_000_000u64;

    let mut sess = make_sess(Some(uid), Some(cid), 0);
    sess.station_id = Some(station_id);
    let mut sess_nostation = make_sess(Some(uid), Some(94_200_002), 0);
    sess_nostation.station_id = None;

    db.with_mut_snapshot(|snap| {
        if !snap.accounts.values().any(|a| a.user_id == uid) {
            let username = format!("test_{uid}");
            snap.accounts.insert(
                username.clone(),
                eve_db::AccountRecord {
                    user_id: uid,
                    username,
                    password_token: String::new(),
                    role: 0,
                    banned_until_unix: None,
                    ban_reason: None,
                },
            );
        }
        snap.account_plex_vault.insert(uid, 5_000);
        let mut ch = snap.characters.get(&90_000_001).cloned().expect("template char");
        ch.character_id = cid;
        ch.user_id = uid;
        ch.clone_grade = 1;
        ch.station_id = Some(station_id);
        ch.home_station_id = station_id;
        snap.characters.insert(cid, ch.clone());

        let mut ch_nostat = ch;
        ch_nostat.character_id = 94_200_002;
        ch_nostat.station_id = None;
        ch_nostat.structure_id = None;
        ch_nostat.home_station_id = 0;
        snap.characters.insert(94_200_002, ch_nostat);

        snap.quasar_commerce.targeted_offers.clear();
        snap.quasar_commerce.payment_tokens = vec![
            PaymentTokenRecord {
                id: 801,
                user_id: uid,
                token_str: "tok-801".into(),
                is_quick_pay: true,
                consumed: false,
            },
            PaymentTokenRecord {
                id: 802,
                user_id: uid,
                token_str: "tok-802".into(),
                is_quick_pay: true,
                consumed: false,
            },
            PaymentTokenRecord {
                id: 803,
                user_id: uid,
                token_str: "tok-803".into(),
                is_quick_pay: true,
                consumed: false,
            },
        ];

        save_store_catalog(
            snap,
            &StoreCatalog {
                categories: Vec::new(),
                products: vec![
                    StoreProduct {
                        id: 8201,
                        name: "Targeted Bundle SKIN".into(),
                        description: "Targeted SKIN".into(),
                        type_id: 34_599,
                        quantity: 1,
                    },
                    StoreProduct {
                        id: 8202,
                        name: "3 Months Omega Clone State".into(),
                        description: "90 days Omega".into(),
                        type_id: 0,
                        quantity: 3,
                    },
                ],
                offers: vec![
                    StoreOffer {
                        id: 8201,
                        name: "starter-pack-alpha".into(),
                        description: "Targeted offer".into(),
                        category_id: 2,
                        price_plex: 100,
                        product_ids: vec![8201],
                        targeted: true,
                        price_cents: Some(499),
                        currency: Some("USD".into()),
                    },
                    StoreOffer {
                        id: 8202,
                        name: "90 Days Omega".into(),
                        description: "Multi-month Omega".into(),
                        category_id: 1,
                        price_plex: 1_200,
                        product_ids: vec![8202],
                        targeted: false,
                        price_cents: Some(4_999),
                        currency: Some("USD".into()),
                    },
                ],
            },
        );
    });

    // 1. BF-02: Even with targeted_offers completely empty, targeted offer 8201 fails closed on Get, Purchase, and Token.
    assert!(db.with_snapshot(|s| s.quasar_commerce.targeted_offers.is_empty()));
    let ta_get_8201 = pb::message("eve_public.store.targetaudience.offer.api.GetRequest")
        .set("offer_id.user_id.sequential", uid as i32)
        .set("offer_id.catalogue_entry.name", "8201")
        .must_encode();
    assert_eq!(call(&router, &sess, "eve_public.store.targetaudience.offer.api.GetRequest", ta_get_8201, now).status_code, STATUS_NOT_FOUND);

    let purch_8201 = WireMessage {
        fields: vec![
            WireField { field_number: 1, value: WireValue::Varint(8201) },
            WireField { field_number: 2, value: WireValue::Varint(1) },
        ],
    }
    .encode();
    assert_eq!(call(&router, &sess, "eve_public.plex.vault.api.PurchaseRequest", purch_8201.clone(), now).status_code, STATUS_FORBIDDEN);

    let tok_8201 = WireMessage {
        fields: vec![
            WireField { field_number: 1, value: WireValue::Varint(8201) },
            WireField { field_number: 2, value: WireValue::Varint(801) },
        ],
    }
    .encode();
    assert_eq!(call(&router, &sess, "eve_public.payment.purchase.api.TokenRequest", tok_8201, now).status_code, STATUS_FORBIDDEN);

    // 2. BF-04: Missing station rejects physical PurchaseRequest & TokenRequest BEFORE debit/token consumption.
    assert_eq!(call(&router, &sess_nostation, "eve_public.plex.vault.api.PurchaseRequest", purch_8201.clone(), now).status_code, STATUS_FORBIDDEN);
    // Promote non-numeric offer name AND test VEVC-8201 resolution in targetaudience GetRequest.
    db.with_mut_snapshot(|snap| {
        snap.quasar_commerce.targeted_offers.insert(
            format!("{uid}:starter-pack-alpha"),
            eve_db::TargetedOfferAdminRecord {
                user_id: uid,
                catalogue_entry_name: "starter-pack-alpha".into(),
                promoted: true,
                retracted: false,
                earliest_reissue_unix: 0,
                partner_code: String::new(),
            },
        );
    });
    let ta_get_named = pb::message("eve_public.store.targetaudience.offer.api.GetRequest")
        .set("offer_id.user_id.sequential", uid as i32)
        .set("offer_id.catalogue_entry.name", "starter-pack-alpha")
        .must_encode();
    assert_eq!(call(&router, &sess, "eve_public.store.targetaudience.offer.api.GetRequest", ta_get_named, now).status_code, STATUS_OK);
    let ta_get_vevc = pb::message("eve_public.store.targetaudience.offer.api.GetRequest")
        .set("offer_id.user_id.sequential", uid as i32)
        .set("offer_id.catalogue_entry.name", "VEVC-8201")
        .must_encode();
    assert_eq!(call(&router, &sess, "eve_public.store.targetaudience.offer.api.GetRequest", ta_get_vevc, now).status_code, STATUS_OK);
    let ta_get_empty = pb::message("eve_public.store.targetaudience.offer.api.GetRequest")
        .set("offer_id.user_id.sequential", uid as i32)
        .set("offer_id.catalogue_entry.name", "")
        .must_encode();
    assert_eq!(call(&router, &sess, "eve_public.store.targetaudience.offer.api.GetRequest", ta_get_empty, now).status_code, STATUS_BAD_REQUEST);

    // Missing station on promoted targeted offer rejects without PLEX deduction or consuming token 801.
    assert_eq!(call(&router, &sess_nostation, "eve_public.plex.vault.api.PurchaseRequest", purch_8201.clone(), now).status_code, STATUS_BAD_REQUEST);
    let tok_8201_nostat = WireMessage {
        fields: vec![
            WireField { field_number: 1, value: WireValue::Varint(8201) },
            WireField { field_number: 2, value: WireValue::Varint(801) },
        ],
    }
    .encode();
    assert_eq!(call(&router, &sess_nostation, "eve_public.payment.purchase.api.TokenRequest", tok_8201_nostat.clone(), now).status_code, STATUS_BAD_REQUEST);
    assert_eq!(db.with_snapshot(|s| s.account_plex_vault.get(&uid).copied()), Some(5_000));
    assert!(db.with_snapshot(|s| !s.quasar_commerce.payment_tokens[0].consumed));

    // 3. Valid physical PurchaseRequest & TokenRequest emit OnItemChange via commit-driven projection.
    ctx.db
        .acquire_online_lock(uid, 942_001, Some(cid), true, now)
        .unwrap();
    let _ = ctx.drain_notifications_for_char(cid);
    assert_eq!(call(&router, &sess, "eve_public.plex.vault.api.PurchaseRequest", purch_8201, now).status_code, STATUS_OK);
    assert_eq!(call(&router, &sess, "eve_public.payment.purchase.api.TokenRequest", tok_8201_nostat, now).status_code, STATUS_OK);
    let on_item_count = ctx
        .drain_notifications_for_char(cid)
        .into_iter()
        .filter(|n| n.event == "OnItemChange")
        .count();
    assert!(on_item_count >= 2, "expected OnItemChange for both PurchaseRequest and TokenRequest, got {on_item_count}");

    // 4. BF-05: 3-month Omega (product.quantity = 3) grants 90 days (90 * 86_400s) and sets schedule interval_days = 90.
    let tok_8202 = WireMessage {
        fields: vec![
            WireField { field_number: 1, value: WireValue::Varint(8202) },
            WireField { field_number: 2, value: WireValue::Varint(802) },
        ],
    }
    .encode();
    assert_eq!(call(&router, &sess, "eve_public.payment.purchase.api.TokenRequest", tok_8202, now).status_code, STATUS_OK);
    let purch_8202 = WireMessage {
        fields: vec![
            WireField { field_number: 1, value: WireValue::Varint(8202) },
            WireField { field_number: 2, value: WireValue::Varint(1) },
        ],
    }
    .encode();
    assert_eq!(call(&router, &sess, "eve_public.plex.vault.api.PurchaseRequest", purch_8202, now).status_code, STATUS_OK);
    db.with_snapshot(|s| {
        assert_eq!(s.characters.get(&cid).map(|c| c.clone_grade), Some(2));
        let sched = s.quasar_commerce.payment_schedules.iter().find(|p| p.user_id == uid).expect("payment schedule");
        assert_eq!(sched.interval_days, 90);
        let id_note = s.owner_notes.values().find(|n| n.label == "S:QUASAR_IDENTITY_STORE").expect("identity store");
        let st: crate::services::quasar_auth_user::QuasarIdentityStore = serde_json::from_str(&id_note.note).unwrap();
        let expiry = st.omega_subscriptions.get(&uid).copied().unwrap_or(0);
        assert!(expiry >= now + 180 * 86_400, "expected >= 180d Omega from two 3-month purchases, got {expiry} (now={now})");
    });
}

#[test]
fn plex_vault_purchase_allows_virtual_omega_without_char_or_station_and_blocks_physical() {
    let router = new_router();
    let db = Arc::clone(&router.db);
    seed_catalog(&db);
    let now = 1_700_000_000u64;

    let uid_charless = 43_001u64;
    let uid_nostat = 43_002u64;
    let cid_nostat = 94_300_002i64;

    db.with_mut_snapshot(|snap| {
        snap.account_plex_vault.insert(uid_charless, 2_000);
        snap.account_plex_vault.insert(uid_nostat, 2_000);
        let mut ch = snap.characters.get(&90_000_001).cloned().expect("template char");
        ch.character_id = cid_nostat;
        ch.user_id = uid_nostat;
        ch.clone_grade = 1;
        ch.station_id = None;
        ch.structure_id = None;
        ch.home_station_id = 0;
        snap.characters.insert(cid_nostat, ch);
    });

    let sess_charless = make_sess(Some(uid_charless), None, 0);
    let mut sess_nostat = make_sess(Some(uid_nostat), Some(cid_nostat), 0);
    sess_nostat.station_id = None;

    let purch_virtual_omega = WireMessage {
        fields: vec![
            WireField { field_number: 1, value: WireValue::Varint(1001) },
            WireField { field_number: 2, value: WireValue::Varint(1) },
        ],
    }
    .encode();
    let purch_physical_skin = WireMessage {
        fields: vec![
            WireField { field_number: 1, value: WireValue::Varint(1002) },
            WireField { field_number: 2, value: WireValue::Varint(1) },
        ],
    }
    .encode();

    // 1. Charless user: physical offer 1002 fails 400 without PLEX deduction; virtual Omega 1001 succeeds.
    assert_eq!(
        call(&router, &sess_charless, "eve_public.plex.vault.api.PurchaseRequest", purch_physical_skin.clone(), now).status_code,
        STATUS_BAD_REQUEST
    );
    assert_eq!(db.with_snapshot(|s| s.account_plex_vault.get(&uid_charless).copied()), Some(2_000));

    assert_eq!(
        call(&router, &sess_charless, "eve_public.plex.vault.api.PurchaseRequest", purch_virtual_omega.clone(), now).status_code,
        STATUS_OK
    );
    db.with_snapshot(|s| {
        assert_eq!(s.account_plex_vault.get(&uid_charless).copied(), Some(1_500));
        let id_note = s.owner_notes.values().find(|n| n.label == "S:QUASAR_IDENTITY_STORE").expect("identity store");
        let st: crate::services::quasar_auth_user::QuasarIdentityStore = serde_json::from_str(&id_note.note).unwrap();
        let expiry = st.omega_subscriptions.get(&uid_charless).copied().unwrap_or(0);
        assert!(expiry >= now + 30 * 86_400, "expected >= 30d Omega for charless account, got {expiry}");
    });

    // 2. Stationless character: physical offer 1002 fails 400 without PLEX deduction; virtual Omega 1001 succeeds.
    assert_eq!(
        call(&router, &sess_nostat, "eve_public.plex.vault.api.PurchaseRequest", purch_physical_skin, now).status_code,
        STATUS_BAD_REQUEST
    );
    assert_eq!(db.with_snapshot(|s| s.account_plex_vault.get(&uid_nostat).copied()), Some(2_000));

    assert_eq!(
        call(&router, &sess_nostat, "eve_public.plex.vault.api.PurchaseRequest", purch_virtual_omega, now).status_code,
        STATUS_OK
    );
    db.with_snapshot(|s| {
        assert_eq!(s.account_plex_vault.get(&uid_nostat).copied(), Some(1_500));
        assert_eq!(s.characters.get(&cid_nostat).map(|c| c.clone_grade), Some(2));
        let id_note = s.owner_notes.values().find(|n| n.label == "S:QUASAR_IDENTITY_STORE").expect("identity store");
        let st: crate::services::quasar_auth_user::QuasarIdentityStore = serde_json::from_str(&id_note.note).unwrap();
        let expiry = st.omega_subscriptions.get(&uid_nostat).copied().unwrap_or(0);
        assert!(expiry >= now + 30 * 86_400, "expected >= 30d Omega for stationless char, got {expiry}");
    });
}


#[cfg(test)]
#[path = "quasar_payment_store_extra_unit_tests.rs"]
mod quasar_payment_store_extra_unit_tests;
