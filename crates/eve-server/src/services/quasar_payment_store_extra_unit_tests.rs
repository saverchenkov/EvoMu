//! Additional unit and mutation-killing tests for `quasar_payment_store`.

#![forbid(unsafe_code)]

use super::super::*;
use crate::quasar_gateway::{
    PublicGatewayRouter, STATUS_BAD_REQUEST, STATUS_FORBIDDEN, STATUS_NOT_FOUND, STATUS_OK,
    STATUS_UNAUTHORIZED,
};
use eve_db::WorldRepository;
use eve_machonet::{service_roles, SessionState};
use eve_proto::pb;
use eve_proto::quasar::{
    EvePublicRequest, EvePublicResponse, GoogleAny, GoogleTimestamp, QuasarDescriptorRegistry,
    WireField, WireMessage, WireValue, TYPE_GOOGLEAPIS_PREFIX,
};
use eve_sde::SdeRegistry;
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

fn shared_descriptors() -> Arc<QuasarDescriptorRegistry> {
    static DESC: OnceLock<Arc<QuasarDescriptorRegistry>> = OnceLock::new();
    Arc::clone(DESC.get_or_init(|| {
        QuasarDescriptorRegistry::open(
            std::path::Path::new("/Users/sergeya/ai/EvoMu/data/quasar_descriptors.sqlite"),
            std::path::Path::new("/Users/sergeya/ai/EvoMu/data/quasar_descriptor_manifest.json"),
        )
        .map(Arc::new)
        .unwrap_or_else(|_| QuasarDescriptorRegistry::open_default_or_empty())
    }))
}

fn new_router() -> (PublicGatewayRouter, Arc<WorldRepository>) {
    let db = Arc::new(WorldRepository::with_demo_seed());
    let router = PublicGatewayRouter::new(shared_descriptors(), Arc::clone(&db), shared_sde());
    (router, db)
}

fn plex_balance(db: &WorldRepository, uid: u64) -> i64 {
    db.with_snapshot(|snap| snap.account_plex_vault.get(&uid).copied().unwrap_or(0))
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

fn call(
    router: &PublicGatewayRouter,
    sess: &SessionState,
    type_name: &str,
    value: Vec<u8>,
    now: u64,
) -> EvePublicResponse {
    let req = EvePublicRequest {
        issued: Some(GoogleTimestamp {
            seconds: now as i64,
            nanos: 0,
        }),
        correlation_uuid: vec![2; 16],
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

fn seed_store_catalog(db: &WorldRepository) {
    db.with_mut_snapshot(|snap| {
        let cat = StoreCatalog {
            categories: vec![StoreCategory {
                id: 1,
                name: "Omega".into(),
                description: "Sub".into(),
            }],
            products: vec![StoreProduct {
                id: 102,
                name: "SKIN".into(),
                description: "Ship SKIN".into(),
                type_id: 34599,
                quantity: 2,
            }],
            offers: vec![
                StoreOffer {
                    id: 1002,
                    name: "Rifter SKIN".into(),
                    description: "SKIN offer".into(),
                    category_id: 1,
                    price_plex: 120,
                    product_ids: vec![102],
                    targeted: true,
                    price_cents: Some(499),
                    currency: Some("EUR".into()),
                },
                StoreOffer {
                    id: 1009,
                    name: "Invalid Negative Offer".into(),
                    description: "Broken".into(),
                    category_id: 1,
                    price_plex: -10,
                    product_ids: vec![102],
                    targeted: false,
                    price_cents: Some(499),
                    currency: Some("USD".into()),
                },
            ],
        };
        save_store_catalog(snap, &cat);
        let tokens = vec![
            PaymentTokenRecord {
                id: 77,
                user_id: 1,
                token_str: "Disabled Card".into(),
                is_quick_pay: false,
                consumed: true,
            },
            PaymentTokenRecord {
                id: 78,
                user_id: 1,
                token_str: "Valid Card".into(),
                is_quick_pay: true,
                consumed: false,
            },
            PaymentTokenRecord {
                id: 79,
                user_id: 1,
                token_str: "Valid Card 2".into(),
                is_quick_pay: true,
                consumed: false,
            },
            PaymentTokenRecord {
                id: 80,
                user_id: 1,
                token_str: "Valid Card 3".into(),
                is_quick_pay: true,
                consumed: false,
            },
        ];
        snap.quasar_commerce.payment_tokens = tokens;
    });
}

#[test]
fn unauthenticated_requests_rejected_across_vault_payment_fulfillment_and_log_endpoints() {
    let (router, _db) = new_router();
    let unauth_none = make_sess(None, None, service_roles::ROLE_GM_DEFAULT);
    let unauth_zero = make_sess(Some(0), None, service_roles::ROLE_GM_DEFAULT);

    let protected_types = [
        "eve_public.plex.vault.api.BalanceRequest",
        "eve.plex.vault.assetholding.operation.api.DepositRequest",
        "eve.plex.vault.assetholding.operation.api.WithdrawRequest",
        "eve_public.store.targetaudience.offer.api.ListRequest",
        "eve_public.payment.schedule.api.GetRequest",
        "eve_public.payment.schedule.api.GetActiveByTypeRequest",
        "eve_public.payment.token.api.GetQuickPayRequest",
        "eve_public.payment.token.api.GetRequest",
        "eve_public.payment.token.api.DisableAllRequest",
        "eve.fulfillment.api.DeliverItemRequest",
        "eve_public.payment.purchase.api.TokenRequest",
        "eve_public.plex.vault.transaction.api.GetAllLoggedForUserRequest",
        "eve_public.plex.vault.transaction.api.GetLogRequest",
        "eve_public.plex.vault.transaction.api.GetStatisticsRequest",
    ];

    for t in protected_types {
        let r1 = call(&router, &unauth_none, t, Vec::new(), 1_700_000_000);
        assert_eq!(r1.status_code, STATUS_UNAUTHORIZED, "expected 401 for {t}");
        let r2 = call(&router, &unauth_zero, t, Vec::new(), 1_700_000_000);
        assert_eq!(
            r2.status_code, STATUS_UNAUTHORIZED,
            "expected 401 for uid=0 on {t}"
        );
    }
}

#[test]
fn deposit_withdraw_default_nested_amounts_and_direct_scalar_catalog_lookups() {
    let (router, db) = new_router();
    seed_store_catalog(&db);
    db.with_mut_snapshot(|s| {
        s.account_plex_vault.insert(1, 0);
    });
    let sess = make_sess(Some(1), Some(90_000_001), service_roles::ROLE_GM_DEFAULT);

    // DepositRequest with empty inner message in field 2 defaults to +100 PLEX
    let dep_empty_nested = WireMessage {
        fields: vec![WireField {
            field_number: 2,
            value: WireValue::LengthDelimited(Vec::new()),
        }],
    }
    .encode();
    let r = call(
        &router,
        &sess,
        "eve.plex.vault.assetholding.operation.api.DepositRequest",
        dep_empty_nested,
        1_700_000_010,
    );
    assert_eq!(r.status_code, STATUS_OK);
    assert_eq!(plex_balance(&db, 1), 100);

    // WithdrawRequest with empty inner message in field 2 defaults to -50 PLEX
    let wd_empty_nested = WireMessage {
        fields: vec![WireField {
            field_number: 2,
            value: WireValue::LengthDelimited(Vec::new()),
        }],
    }
    .encode();
    let r = call(
        &router,
        &sess,
        "eve.plex.vault.assetholding.operation.api.WithdrawRequest",
        wd_empty_nested,
        1_700_000_020,
    );
    assert_eq!(r.status_code, STATUS_OK);
    assert_eq!(plex_balance(&db, 1), 50);

    // Nested message field 1 lookups for Category, Product, Offer, TargetAudience Offer
    let direct_cat = pb::message("eve_public.store.category.api.GetRequest")
        .set("category.sequential", 1u32)
        .must_encode();
    assert_eq!(
        call(
            &router,
            &sess,
            "eve_public.store.category.api.GetRequest",
            direct_cat,
            1_700_000_030
        )
        .status_code,
        STATUS_OK
    );

    let direct_prod = pb::message("eve_public.store.product.api.GetRequest")
        .set("product.sequential", 102u32)
        .must_encode();
    assert_eq!(
        call(
            &router,
            &sess,
            "eve_public.store.product.api.GetRequest",
            direct_prod,
            1_700_000_031
        )
        .status_code,
        STATUS_OK
    );

    let direct_offer = pb::message("eve_public.store.offer.api.GetRequest")
        .set("offer.store_offer", "1002")
        .must_encode();
    assert_eq!(
        call(
            &router,
            &sess,
            "eve_public.store.offer.api.GetRequest",
            direct_offer,
            1_700_000_032
        )
        .status_code,
        STATUS_OK
    );
    let direct_ta_offer = pb::message("eve_public.store.targetaudience.offer.api.GetRequest")
        .set("offer_id.catalogue_entry.name", "1002")
        .must_encode();
    assert_eq!(
        call(
            &router,
            &sess,
            "eve_public.store.targetaudience.offer.api.GetRequest",
            direct_ta_offer,
            1_700_000_033
        )
        .status_code,
        STATUS_OK
    );
}

#[test]
fn deliver_item_fallbacks_default_type_qty_and_station_resolution() {
    let (router, db) = new_router();
    // 1. Session without character_id falls back to get_characters_for_user(1) -> 90_000_001
    // Also omit fields 2 and 3 so type_id defaults to 44992 and qty defaults to 1
    let sess_no_cid = make_sess(Some(1), None, service_roles::ROLE_GM_DEFAULT);
    let r = call(
        &router,
        &sess_no_cid,
        "eve.fulfillment.api.DeliverItemRequest",
        Vec::new(),
        1_700_000_100,
    );
    assert_eq!(r.status_code, STATUS_OK);

    let delivered_default = db.with_snapshot(|snap| {
        snap.items
            .values()
            .any(|it| it.owner_id == 90_000_001 && it.type_id == 44_992 && it.quantity == 1)
    });
    assert!(delivered_default);

    // 2. Non-existent target character ID in session -> STATUS_NOT_FOUND
    let sess_bad_cid = make_sess(Some(1), Some(999_999_999), service_roles::ROLE_GM_DEFAULT);
    let r = call(
        &router,
        &sess_bad_cid,
        "eve.fulfillment.api.DeliverItemRequest",
        Vec::new(),
        1_700_000_101,
    );
    assert_eq!(r.status_code, STATUS_NOT_FOUND);

    // 3. Undocked character with structure_id receives item at structure_id
    db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            ch.station_id = None;
            ch.structure_id = Some(1_000_000_777_001);
        }
    });
    let sess = make_sess(Some(1), Some(90_000_001), service_roles::ROLE_GM_DEFAULT);
    let r = call(
        &router,
        &sess,
        "eve.fulfillment.api.DeliverItemRequest",
        Vec::new(),
        1_700_000_102,
    );
    assert_eq!(r.status_code, STATUS_OK);
    let at_struct = db.with_snapshot(|snap| {
        snap.items.values().any(|it| {
            it.owner_id == 90_000_001
                && it.location_id == 1_000_000_777_001
                && it.type_id == 44_992
        })
    });
    assert!(at_struct);
}

#[test]
fn cost_and_token_purchase_prefix_stripping_negative_price_and_recipient_validation() {
    let (router, db) = new_router();
    seed_store_catalog(&db);
    let sess = make_sess(Some(1), Some(90_000_001), service_roles::ROLE_PLAYER_DEFAULT);

    // CostRequest for offer 1002 -> STATUS_OK
    let cost_1002 = pb::message("eve_public.payment.purchase.api.CostRequest")
        .set("catalog_amount_in_cents", 1002u32)
        .must_encode();
    let r = call(
        &router,
        &sess,
        "eve_public.payment.purchase.api.CostRequest",
        cost_1002,
        1_700_000_200,
    );
    assert_eq!(r.status_code, STATUS_OK);

    // TokenRequest for offer 1009 with negative price_plex -> STATUS_BAD_REQUEST
    let neg_tok = pb::message("eve_public.payment.purchase.api.TokenRequest")
        .set("order.offer.store_offer", "1009")
        .set("token.sequential", 78u32)
        .must_encode();
    let r = call(
        &router,
        &sess,
        "eve_public.payment.purchase.api.TokenRequest",
        neg_tok,
        1_700_000_202,
    );
    assert_eq!(r.status_code, STATUS_BAD_REQUEST);

    // TokenRequest with disabled payment_token_id (77) -> STATUS_BAD_REQUEST
    let disabled_tok_req = pb::message("eve_public.payment.purchase.api.TokenRequest")
        .set("order.offer.store_offer", "1002")
        .set("token.sequential", 77u32)
        .must_encode();
    let r = call(
        &router,
        &sess,
        "eve_public.payment.purchase.api.TokenRequest",
        disabled_tok_req,
        1_700_000_203,
    );
    assert_eq!(r.status_code, STATUS_BAD_REQUEST);

    // TokenRequest with non-existent active_char_id -> STATUS_NOT_FOUND
    let sess_missing_cid = make_sess(Some(1), Some(999_888_777), service_roles::ROLE_PLAYER_DEFAULT);
    let valid_tok_req = pb::message("eve_public.payment.purchase.api.TokenRequest")
        .set("order.offer.store_offer", "1002")
        .set("token.sequential", 78u32)
        .must_encode();
    let r = call(
        &router,
        &sess_missing_cid,
        "eve_public.payment.purchase.api.TokenRequest",
        valid_tok_req.clone(),
        1_700_000_204,
    );
    assert_eq!(r.status_code, STATUS_NOT_FOUND);

    // TokenRequest with active_char_id belonging to another user_id -> STATUS_FORBIDDEN
    db.with_mut_snapshot(|snap| {
        if let Some(ch2) = snap.characters.get_mut(&90_000_002) {
            ch2.user_id = 999;
        }
    });
    let sess_wrong_owner = make_sess(Some(1), Some(90_000_002), service_roles::ROLE_PLAYER_DEFAULT);
    let r = call(
        &router,
        &sess_wrong_owner,
        "eve_public.payment.purchase.api.TokenRequest",
        valid_tok_req.clone(),
        1_700_000_205,
    );
    assert_eq!(r.status_code, STATUS_FORBIDDEN);

    // TokenRequest where recipient character has no valid station/structure/home_station -> STATUS_BAD_REQUEST
    db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            ch.station_id = None;
            ch.structure_id = None;
            ch.home_station_id = 0;
        }
    });
    let r = call(
        &router,
        &sess,
        "eve_public.payment.purchase.api.TokenRequest",
        valid_tok_req,
        1_700_000_206,
    );
    assert_eq!(r.status_code, STATUS_BAD_REQUEST);
}

#[test]
fn schedule_token_quickpay_and_purchase_idor_and_predicate_isolation() {
    let (router, db) = new_router();
    seed_store_catalog(&db);
    let u1_sess = make_sess(Some(1), Some(90_000_001), service_roles::ROLE_PLAYER_DEFAULT);
    let u2_sess = make_sess(Some(2), None, service_roles::ROLE_PLAYER_DEFAULT);

    // Seed schedules in order: other-user same-type FIRST, same-user other-type SECOND, target THIRD
    // so flipping any predicate clause (`&& -> ||`, `id != sched_id`, `user_id != uid`, `schedule_type != stype`) picks a wrong record.
    db.with_mut_snapshot(|snap| {
        snap.quasar_commerce.payment_schedules = vec![
            eve_db::PaymentScheduleRecord {
                id: 102,
                user_id: 2,
                schedule_type: 1,
                next_payment_unix: 1_700_086_400,
                offer_id: "offer-u2-s1".into(),
                interval_days: 30,
                created_unix: 1_700_000_000,
            },
            eve_db::PaymentScheduleRecord {
                id: 103,
                user_id: 1,
                schedule_type: 2,
                next_payment_unix: 1_700_086_400,
                offer_id: "offer-u1-s2".into(),
                interval_days: 60,
                created_unix: 1_700_000_000,
            },
            eve_db::PaymentScheduleRecord {
                id: 101,
                user_id: 1,
                schedule_type: 1,
                next_payment_unix: 1_700_086_400,
                offer_id: "offer-u1-s1".into(),
                interval_days: 90,
                created_unix: 1_700_000_000,
            },
        ];

        // Tokens in order: u2 quick-pay FIRST, u1 non-quick-pay SECOND, u1 quick-pay THIRD
        snap.quasar_commerce.payment_tokens = vec![
            PaymentTokenRecord {
                id: 702,
                user_id: 2,
                token_str: "u2-card-702".into(),
                is_quick_pay: true,
                consumed: false,
            },
            PaymentTokenRecord {
                id: 703,
                user_id: 1,
                token_str: "u1-disabled-703".into(),
                is_quick_pay: false,
                consumed: false,
            },
            PaymentTokenRecord {
                id: 701,
                user_id: 1,
                token_str: "u1-valid-701".into(),
                is_quick_pay: true,
                consumed: false,
            },
        ];
    });

    // 1. schedule.api.GetRequest: u1 gets 101 ("offer-u1-s1"), NOT 103; cross-user 102 is NOT_FOUND
    let mk_nested_id = |id: u64| -> Vec<u8> {
        pb::message("eve_public.payment.schedule.api.GetRequest")
            .set("schedule.sequential", id)
            .must_encode()
    };
    let mk_token_id = |id: u32| -> Vec<u8> {
        pb::message("eve_public.payment.token.api.GetRequest")
            .set("token.sequential", id)
            .must_encode()
    };
    let r101 = call(
        &router,
        &u1_sess,
        "eve_public.payment.schedule.api.GetRequest",
        mk_nested_id(101),
        1_700_000_300,
    );
    assert_eq!(r101.status_code, STATUS_OK);
    let bytes_101 = r101.payload.expect("101 payload").value;
    assert!(bytes_101.windows(b"offer-u1-s1".len()).any(|w| w == b"offer-u1-s1"));
    assert!(!bytes_101.windows(b"offer-u1-s2".len()).any(|w| w == b"offer-u1-s2"));

    assert_eq!(
        call(
            &router,
            &u1_sess,
            "eve_public.payment.schedule.api.GetRequest",
            mk_nested_id(102),
            1_700_000_301,
        )
        .status_code,
        STATUS_NOT_FOUND
    );
    assert_eq!(
        call(
            &router,
            &u2_sess,
            "eve_public.payment.schedule.api.GetRequest",
            mk_nested_id(102),
            1_700_000_302,
        )
        .status_code,
        STATUS_OK
    );

    // 2. schedule.api.GetActiveByTypeRequest: u1 type 1 returns id 101 (NOT 102 or 103); type 9 is NOT_FOUND
    let stype_1 = pb::message("eve_public.payment.schedule.api.GetActiveByTypeRequest")
        .set("type", 1i32)
        .must_encode();
    let r_act = call(
        &router,
        &u1_sess,
        "eve_public.payment.schedule.api.GetActiveByTypeRequest",
        stype_1,
        1_700_000_303,
    );
    assert_eq!(r_act.status_code, STATUS_OK);
    assert_eq!(r_act.payload.expect("active sched").value, mk_nested_id(101));

    let stype_9 = pb::message("eve_public.payment.schedule.api.GetActiveByTypeRequest")
        .set("type", 9i32)
        .must_encode();
    assert_eq!(
        call(
            &router,
            &u1_sess,
            "eve_public.payment.schedule.api.GetActiveByTypeRequest",
            stype_9,
            1_700_000_304,
        )
        .status_code,
        STATUS_NOT_FOUND
    );

    // 3. payment.token.api.GetQuickPayRequest: u1 receives ONLY token 701 (excludes u2's 702 and non-QP 703)
    let r_qp = call(
        &router,
        &u1_sess,
        "eve_public.payment.token.api.GetQuickPayRequest",
        Vec::new(),
        1_700_000_305,
    );
    assert_eq!(r_qp.status_code, STATUS_OK);
    assert_eq!(r_qp.payload.expect("quick pay").value, mk_token_id(701));

    // 4. payment.token.api.GetRequest: u1 gets 701 ("u1-valid-701"), NOT 703; u1 requesting u2's 702 -> NOT_FOUND
    let r_tok = call(
        &router,
        &u1_sess,
        "eve_public.payment.token.api.GetRequest",
        mk_token_id(701),
        1_700_000_306,
    );
    assert_eq!(r_tok.status_code, STATUS_OK);
    let tok_bytes = r_tok.payload.expect("tok payload").value;
    assert!(tok_bytes.windows(b"u1-valid-701".len()).any(|w| w == b"u1-valid-701"));
    assert!(!tok_bytes.windows(b"u1-disabled-703".len()).any(|w| w == b"u1-disabled-703"));
    assert_eq!(
        call(
            &router,
            &u1_sess,
            "eve_public.payment.token.api.GetRequest",
            mk_token_id(702),
            1_700_000_307,
        )
        .status_code,
        STATUS_NOT_FOUND
    );

    // 5. TokenRequest: cross-user token 702 -> FORBIDDEN; non-quick-pay 703 -> BAD_REQUEST; valid 701 -> OK
    let mk_tok_purchase = |tok_id: u64| -> Vec<u8> {
        pb::message("eve_public.payment.purchase.api.TokenRequest")
            .set("order.offer.store_offer", "1002")
            .set("token.sequential", tok_id as u32)
            .must_encode()
    };
    assert_eq!(
        call(
            &router,
            &u1_sess,
            "eve_public.payment.purchase.api.TokenRequest",
            mk_tok_purchase(702),
            1_700_000_308,
        )
        .status_code,
        STATUS_FORBIDDEN
    );
    assert_eq!(
        call(
            &router,
            &u1_sess,
            "eve_public.payment.purchase.api.TokenRequest",
            mk_tok_purchase(703),
            1_700_000_309,
        )
        .status_code,
        STATUS_BAD_REQUEST
    );
    assert_eq!(
        call(
            &router,
            &u1_sess,
            "eve_public.payment.purchase.api.TokenRequest",
            mk_tok_purchase(701),
            1_700_000_310,
        )
        .status_code,
        STATUS_OK
    );

    // 6. DisableAllRequest as u1 disables ONLY u1's active quick-pay token and leaves u2's 702 quick-pay intact
    db.with_mut_snapshot(|snap| {
        snap.quasar_commerce.payment_tokens.push(PaymentTokenRecord {
            id: 705,
            user_id: 1,
            token_str: "u1-fresh-705".into(),
            is_quick_pay: true,
            consumed: false,
        });
    });
    router.pending_notices.write().unwrap().clear();
    router.routing_cache.write().unwrap().entries.clear();
    assert_eq!(
        call(
            &router,
            &u1_sess,
            "eve_public.payment.token.api.DisableAllRequest",
            Vec::new(),
            1_700_000_311,
        )
        .status_code,
        STATUS_OK
    );
    let notice_payloads: Vec<Vec<u8>> = router
        .routing_cache
        .read()
        .unwrap()
        .entries
        .iter()
        .filter_map(|e| e.notice.payload.as_ref().map(|p| p.value.clone()))
        .collect();
    assert_eq!(notice_payloads.len(), 2);
    assert_eq!(notice_payloads, vec![mk_nested_id(701), mk_nested_id(705)]);
    db.with_snapshot(|snap| {
        assert!(!snap.quasar_commerce.payment_tokens.iter().find(|t| t.id == 701).unwrap().is_quick_pay);
        assert!(!snap.quasar_commerce.payment_tokens.iter().find(|t| t.id == 705).unwrap().is_quick_pay);
        assert!(snap.quasar_commerce.payment_tokens.iter().find(|t| t.id == 702).unwrap().is_quick_pay);
    });
}
