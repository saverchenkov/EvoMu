//! State-backed integration tests for the Paragon Hub Cosmetic Market (`quasar_cosmetics_market`),
//! `OF-16` (`contractMgr.BreakPlasticWrap` docking/location check), and `OF-31` (per-entity
//! `CombatState.rng_state` seeding).

use crate::quasar_goal_tale_notices::{bytes, drain, seq, sub, subscribe, varint};
use crate::quasar_progression_complete::{build_sess, quasar_rpc};
use eve_db::{InventoryFlag, InventoryItemRecord, IskCents};
use eve_machonet::SessionState;
use eve_proto::quasar::{
    encode_bytes_field, encode_string_field, encode_varint_field, DynamicValue,
    QuasarDescriptorRegistry, WireMessage,
};
use eve_proto::PyRep;
use eve_server::quasar_gateway::{
    STATUS_BAD_REQUEST, STATUS_FORBIDDEN, STATUS_NOT_FOUND, STATUS_OK, STATUS_PRECONDITION_FAILED,
};
use eve_server::router::ServerContext;
use eve_server::services::quasar_cosmetics_market::{
    skin_tier_for_hex, BUYER_FEES_VERSION, PARAGON_CORP_ID,
};
use std::collections::HashMap;

const CORP_A: u32 = 98_000_901;
const CORP_B: u32 = 98_000_902;

fn player(sid: u64, cid: u32, corp: u32) -> SessionState {
    let mut s = build_sess(sid, cid, corp);
    s.user_id = Some(sid);
    s.role = eve_machonet::service_roles::ROLE_PLAYER_DEFAULT;
    s
}

fn seed_char(ctx: &ServerContext, cid: i64, corp: u32, isk: i64, plex: i64) {
    ctx.db.with_mut_snapshot(|s| {
        let template = s.characters[&90_000_001].clone();
        let ch = s.characters.entry(cid).or_insert_with(|| template);
        ch.character_id = cid;
        ch.corporation_id = corp as i64;
        ch.balance = IskCents::from_isk(isk);
        ch.plex_balance = plex;
        ch.loyalty_points.insert(PARAGON_CORP_ID as i64, 50_000);
        ch.station_id = Some(60_003_760);
    });
}

fn decode(rsp: &eve_proto::quasar::EvePublicResponse) -> WireMessage {
    WireMessage::decode(&rsp.payload.as_ref().expect("payload").value).expect("decode")
}

fn uuid_field(field: u32, uuid: &[u8], out: &mut Vec<u8>) {
    let mut b = Vec::new();
    encode_bytes_field(1, uuid, &mut b);
    encode_bytes_field(field, &b, out);
}

#[tokio::test]
async fn paragon_hub_cosmetic_market_fees_and_component_listings() {
    // parity: quasar:eveProto/generated/eve_public/cosmetic/market/api/api_pb2.pyj
    // parity: quasar:eveProto/generated/eve_public/cosmetic/market/component/listing/api/api_pb2.pyj
    eve_testkit::attest(&[
        "quasar:eveProto/generated/eve_public/cosmetic/market/api/api_pb2.pyj",
        "quasar:eveProto/generated/eve_public/cosmetic/market/component/listing/api/api_pb2.pyj",
    ]);

    let reg = QuasarDescriptorRegistry::open_default().expect("registry");
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let buyer_cid = 90_000_411u32;
    seed_char(&ctx, buyer_cid as i64, CORP_A, 2_000_000, 100);
    let mut buyer = player(71_001, buyer_cid, CORP_A);
    let mut rx = subscribe(&ctx, &mut buyer).await;

    // 1. GetBrokerFeesRequest: public vs targeted corporation listing.
    let mut pub_target = Vec::new();
    encode_varint_field(1, 1, &mut pub_target);
    let mut req_pub = Vec::new();
    encode_bytes_field(1, &pub_target, &mut req_pub);
    let rsp_bf_pub = quasar_rpc(&ctx, &mut buyer, "eve_public.cosmetic.market.api.GetBrokerFeesRequest", req_pub.clone()).await;
    assert_eq!(rsp_bf_pub.status_code, STATUS_OK);
    let wm_bf_pub = decode(&rsp_bf_pub);
    let per_dur_pub: Vec<_> = wm_bf_pub.fields.iter().filter(|f| f.field_number == 2).collect();
    assert_eq!(per_dur_pub.len(), 6, "6 standard Paragon listing durations");
    let first_pub_fee = varint(&sub(&sub(&sub(&decode(&rsp_bf_pub), 2), 2), 2), 1).expect("pub fee");

    let mut corp_target = Vec::new();
    seq(2, CORP_A as u64, &mut corp_target);
    let mut req_corp = Vec::new();
    encode_bytes_field(1, &corp_target, &mut req_corp);
    let rsp_bf_corp = quasar_rpc(&ctx, &mut buyer, "eve_public.cosmetic.market.api.GetBrokerFeesRequest", req_corp.clone()).await;
    assert_eq!(rsp_bf_corp.status_code, STATUS_OK);
    let first_corp_fee = varint(&sub(&sub(&sub(&decode(&rsp_bf_corp), 2), 2), 2), 1).expect("corp fee");
    assert_eq!(first_corp_fee * 2, first_pub_fee, "Branded listing broker fee is 50% of public");

    // 2. GetTaxRateRequest: sint64 units = 20 (public) vs 10 (branded).
    let rsp_tr_pub = quasar_rpc(&ctx, &mut buyer, "eve_public.cosmetic.market.api.GetTaxRateRequest", req_pub).await;
    assert_eq!(rsp_tr_pub.status_code, STATUS_OK);
    let dyn_tr_pub = reg.decode_dynamic("eve_public.cosmetic.market.api.GetTaxRateResponse", &rsp_tr_pub.payload.as_ref().unwrap().value).expect("decode GetTaxRateResponse");
    if let Some(DynamicValue::Message(tr)) = dyn_tr_pub.get(1) {
        assert_eq!(tr.get(1), Some(&DynamicValue::Sint64(20)));
    } else {
        panic!("Expected tax_rate message");
    }
    let rsp_tr_corp = quasar_rpc(&ctx, &mut buyer, "eve_public.cosmetic.market.api.GetTaxRateRequest", req_corp.clone()).await;
    let dyn_tr_corp = reg.decode_dynamic("eve_public.cosmetic.market.api.GetTaxRateResponse", &rsp_tr_corp.payload.as_ref().unwrap().value).expect("decode GetTaxRateResponse");
    if let Some(DynamicValue::Message(tr)) = dyn_tr_corp.get(1) {
        assert_eq!(tr.get(1), Some(&DynamicValue::Sint64(10)));
    }

    // 3. GetBuyerFeeRequest & GetBuyerFeesDefinitionRequest.
    let mut req_buyer_fee = req_corp;
    let mut skin_id = Vec::new();
    encode_string_field(1, "cafebabe1234", &mut skin_id);
    encode_bytes_field(2, &skin_id, &mut req_buyer_fee);
    let rsp_buyer_fee = quasar_rpc(&ctx, &mut buyer, "eve_public.cosmetic.market.api.GetBuyerFeeRequest", req_buyer_fee).await;
    assert_eq!(rsp_buyer_fee.status_code, STATUS_OK);
    let lp_msg = sub(&sub(&decode(&rsp_buyer_fee), 1), 1);
    assert_eq!(varint(&sub(&lp_msg, 1), 1), Some(PARAGON_CORP_ID));
    assert!(varint(&lp_msg, 2).unwrap_or(0) >= 2_500);

    let rsp_defs = quasar_rpc(&ctx, &mut buyer, "eve_public.cosmetic.market.api.GetBuyerFeesDefinitionRequest", Vec::new()).await;
    assert_eq!(rsp_defs.status_code, STATUS_OK);
    let wm_defs = decode(&rsp_defs);
    assert_eq!(wm_defs.fields.iter().filter(|f| f.field_number == 1).count(), 5);
    assert_eq!(std::str::from_utf8(&bytes(&wm_defs, 2)).unwrap(), BUYER_FEES_VERSION);

    // Verify custom skin tier lookup from SkinrCosmeticsState when design exists
    let skinr = eve_server::services::quasar_cosmetics_skinr::SkinrCosmeticsState::load(std::sync::Arc::clone(&ctx.db), std::sync::Arc::clone(&ctx.sde));
    let mkt = eve_server::services::quasar_cosmetics_market::CosmeticsMarketState::load(std::sync::Arc::clone(&ctx.db), std::sync::Arc::clone(&ctx.sde), std::sync::Arc::clone(&skinr));
    skinr.data.write().unwrap().skins.insert("custom_hex_4".into(), eve_server::services::quasar_cosmetics_skinr::SkinDesignRecord {
        skin_hex: "custom_hex_4".into(), name: "Custom".into(), line_name: "L".into(), creator_id: 1, ship_type_id: 587, pattern_blend_mode: 0, slots: Vec::new(), tier_level: 4, first_sequenced_unix: None,
    });
    assert_eq!(mkt.skin_tier_for_hex("custom_hex_4"), 4);
    assert_eq!(mkt.skin_tier_for_hex("nonexistent"), skin_tier_for_hex("nonexistent"));

    // 4. Component listings GetAllRequest + InitiatePurchaseListingRequest.
    let rsp_comp_all = quasar_rpc(&ctx, &mut buyer, "eve_public.cosmetic.market.component.listing.api.GetAllRequest", Vec::new()).await;
    assert_eq!(rsp_comp_all.status_code, STATUS_OK);
    let wm_comp = decode(&rsp_comp_all);
    let entries: Vec<_> = wm_comp.fields.iter().filter(|f| f.field_number == 1).collect();
    assert_eq!(entries.len(), 4, "4 seeded Ship SKIN Design Element listings");
    let first_entry = sub(&wm_comp, 1);
    let first_uuid = bytes(&sub(&first_entry, 1), 1);
    let first_attrs = sub(&first_entry, 2);
    let comp_type_id = varint(&sub(&first_attrs, 5), 1).expect("comp_type_id") as u32;
    assert_eq!(
        ctx.sde.types.get(&comp_type_id).map(|t| t.group_id),
        Some(4726),
        "Seeded component listing must reference an authentic SDE group 4726 type"
    );

    let buyer_pre_overflow = ctx.db.with_snapshot(|s| s.characters[&(buyer_cid as i64)].balance);
    let mut overflow_buy_req = Vec::new();
    uuid_field(1, &first_uuid, &mut overflow_buy_req);
    encode_varint_field(2, u64::MAX, &mut overflow_buy_req);
    let rsp_overflow = quasar_rpc(
        &ctx,
        &mut buyer,
        "eve_public.cosmetic.market.component.listing.api.InitiatePurchaseListingRequest",
        overflow_buy_req,
    )
    .await;
    assert_eq!(rsp_overflow.status_code, STATUS_BAD_REQUEST);
    assert_eq!(
        ctx.db.with_snapshot(|s| s.characters[&(buyer_cid as i64)].balance),
        buyer_pre_overflow,
        "Buyer balance must remain untouched on component overflow rejection"
    );

    let mut buy_req = Vec::new();
    uuid_field(1, &first_uuid, &mut buy_req);
    encode_varint_field(2, 2, &mut buy_req); // quantity = 2 (2 * 250,000 ISK = 500,000 ISK)
    encode_varint_field(3, 1, &mut buy_req); // immediately_activate = true
    let rsp_buy = quasar_rpc(
        &ctx,
        &mut buyer,
        "eve_public.cosmetic.market.component.listing.api.InitiatePurchaseListingRequest",
        buy_req,
    )
    .await;
    assert_eq!(rsp_buy.status_code, STATUS_OK);
    let wm_buy = decode(&rsp_buy);
    let tx_uuid = bytes(&sub(&wm_buy, 1), 1);
    assert_eq!(tx_uuid.len(), 16);
    assert_eq!(varint(&wm_buy, 2), Some(1));

    let notices = drain(&mut rx, "eve_public.cosmetic.market.transaction.api");
    assert!(
        notices.iter().any(|(name, w)| {
            name == "eve_public.cosmetic.market.transaction.api.TransactionSucceededNotice"
                && bytes(&sub(w, 1), 1) == tx_uuid
        }),
        "Expected TransactionSucceededNotice for component purchase"
    );
    ctx.db.with_snapshot(|s| {
        assert_eq!(
            s.characters[&(buyer_cid as i64)].balance,
            IskCents::from_isk(1_500_000)
        );
        let owned_qty: i32 = s
            .items
            .values()
            .filter(|i| i.owner_id == buyer_cid as i64 && i.type_id == comp_type_id)
            .map(|i| i.quantity)
            .sum();
        assert_eq!(owned_qty, 2);
    });
}

#[tokio::test]
async fn paragon_hub_skin_listings_lifecycle_admin_and_transaction_notices() {
    // parity: quasar:eveProto/generated/eve_public/cosmetic/market/skin/listing/api/api_pb2.pyj
    // parity: quasar:eveProto/generated/eve_public/cosmetic/market/skin/listing/api/admin/requests_pb2.pyj
    // parity: quasar:eveProto/generated/eve_public/cosmetic/market/transaction/api/notices_pb2.pyj
    eve_testkit::attest(&[
        "quasar:eveProto/generated/eve_public/cosmetic/market/skin/listing/api/api_pb2.pyj",
        "quasar:eveProto/generated/eve_public/cosmetic/market/skin/listing/api/admin/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/cosmetic/market/transaction/api/notices_pb2.pyj",
    ]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let seller_cid = 90_000_421u32;
    let buyer_cid = 90_000_422u32;
    let poor_cid = 90_000_423u32;
    seed_char(&ctx, seller_cid as i64, CORP_A, 1_000_000, 0);
    seed_char(&ctx, buyer_cid as i64, CORP_B, 5_000_000, 0);
    seed_char(&ctx, poor_cid as i64, CORP_B, 100, 0);

    let mut seller = player(72_001, seller_cid, CORP_A);
    let mut buyer = player(72_002, buyer_cid, CORP_B);
    let mut poor_buyer = player(72_003, poor_cid, CORP_B);
    let mut gm = build_sess(72_004, 90_000_001, CORP_A);
    let mut seller_gm = build_sess(72_005, seller_cid, CORP_A);

    let mut seller_rx = subscribe(&ctx, &mut seller).await;
    let mut buyer_rx = subscribe(&ctx, &mut buyer).await;
    let mut poor_rx = subscribe(&ctx, &mut poor_buyer).await;

    // 1. Negative validation on InitiateCreateListingRequest (empty -> 400, unowned skin -> 412, overflow qty -> 400).
    let bad_create = quasar_rpc(
        &ctx,
        &mut seller,
        "eve_public.cosmetic.market.skin.listing.api.InitiateCreateListingRequest",
        Vec::new(),
    )
    .await;
    assert_eq!(bad_create.status_code, STATUS_BAD_REQUEST);

    // 2. Seller creates a public listing for 3x "deadbeefcafebabe" at 500,000 ISK each for 1 day.
    let build_create_req = |skin_hex: &str, qty: u64| {
        let mut price_isk = Vec::new();
        encode_varint_field(1, 500_000 * 2, &mut price_isk); // sint64 zigzag of 500_000
        let mut skin_id = Vec::new();
        encode_string_field(1, skin_hex, &mut skin_id);
        let mut lic_id = Vec::new();
        seq(1, seller_cid as u64, &mut lic_id);
        encode_bytes_field(2, &skin_id, &mut lic_id);
        let mut dur = Vec::new();
        encode_varint_field(1, 86_400, &mut dur);
        let mut target = Vec::new();
        encode_varint_field(1, 1, &mut target); // public = true
        let mut req = Vec::new();
        encode_bytes_field(1, &price_isk, &mut req);
        encode_bytes_field(3, &lic_id, &mut req);
        encode_bytes_field(4, &dur, &mut req);
        encode_varint_field(5, qty, &mut req);
        encode_bytes_field(6, &target, &mut req);
        req
    };
    let rsp_unowned = quasar_rpc(
        &ctx,
        &mut seller,
        "eve_public.cosmetic.market.skin.listing.api.InitiateCreateListingRequest",
        build_create_req("deadbeefcafebabe", 3),
    )
    .await;
    assert_eq!(rsp_unowned.status_code, STATUS_PRECONDITION_FAILED);
    let rsp_overflow_qty = quasar_rpc(
        &ctx,
        &mut seller,
        "eve_public.cosmetic.market.skin.listing.api.InitiateCreateListingRequest",
        build_create_req("deadbeefcafebabe", u64::MAX),
    )
    .await;
    assert_eq!(rsp_overflow_qty.status_code, STATUS_BAD_REQUEST);

    for (hex, count) in [("deadbeefcafebabe", 4u64), ("feedface00112233", 2u64)] {
        let mut grant_req = Vec::new();
        let mut skin_id = Vec::new();
        encode_string_field(1, hex, &mut skin_id);
        encode_bytes_field(1, &skin_id, &mut grant_req);
        encode_varint_field(2, count, &mut grant_req);
        let rsp_grant = quasar_rpc(
            &ctx,
            &mut seller_gm,
            "eve_public.cosmetic.ship.skin.thirdparty.license.api.admin.GrantRequest",
            grant_req,
        )
        .await;
        assert_eq!(rsp_grant.status_code, STATUS_OK);
    }

    let rsp_create = quasar_rpc(
        &ctx,
        &mut seller,
        "eve_public.cosmetic.market.skin.listing.api.InitiateCreateListingRequest",
        build_create_req("deadbeefcafebabe", 3),
    )
    .await;
    assert_eq!(rsp_create.status_code, STATUS_OK);
    let create_tx_uuid = bytes(&sub(&decode(&rsp_create), 1), 1);
    assert_eq!(create_tx_uuid.len(), 16);
    let seller_notices = drain(&mut seller_rx, "eve_public.cosmetic.market.transaction.api");
    assert!(
        seller_notices.iter().any(|(n, w)| {
            n == "eve_public.cosmetic.market.transaction.api.TransactionSucceededNotice"
                && bytes(&sub(w, 1), 1) == create_tx_uuid
        }),
        "Seller must receive TransactionSucceededNotice on listing creation"
    );

    // 3. GetAllOwnedRequest & GetAllRequest & GetRequest return the active listing.
    let rsp_owned = quasar_rpc(
        &ctx,
        &mut seller,
        "eve_public.cosmetic.market.skin.listing.api.GetAllOwnedRequest",
        Vec::new(),
    )
    .await;
    assert_eq!(rsp_owned.status_code, STATUS_OK);
    let entry = sub(&decode(&rsp_owned), 1);
    let listing_uuid = bytes(&sub(&entry, 1), 1);
    let attrs = sub(&entry, 2);
    assert_eq!(varint(&sub(&attrs, 2), 1), Some(seller_cid as u64));
    assert_eq!(varint(&attrs, 8), Some(3));

    let mut get_req = Vec::new();
    uuid_field(1, &listing_uuid, &mut get_req);
    let rsp_get = quasar_rpc(
        &ctx,
        &mut buyer,
        "eve_public.cosmetic.market.skin.listing.api.GetRequest",
        get_req.clone(),
    )
    .await;
    assert_eq!(rsp_get.status_code, STATUS_OK);
    assert_eq!(varint(&sub(&sub(&decode(&rsp_get), 1), 2), 8), Some(3));

    // 4. Poor buyer with insufficient ISK gets TransactionFailedNotice; listing quantity stays 3.
    let mut buy_req = Vec::new();
    uuid_field(1, &listing_uuid, &mut buy_req);
    encode_varint_field(2, 2, &mut buy_req);
    encode_string_field(3, BUYER_FEES_VERSION, &mut buy_req);
    let rsp_poor_buy = quasar_rpc(
        &ctx,
        &mut poor_buyer,
        "eve_public.cosmetic.market.skin.listing.api.InitiatePurchaseListingRequest",
        buy_req.clone(),
    )
    .await;
    assert_eq!(rsp_poor_buy.status_code, STATUS_OK);
    let failed_tx_uuid = bytes(&sub(&decode(&rsp_poor_buy), 1), 1);
    let poor_notices = drain(&mut poor_rx, "eve_public.cosmetic.market.transaction.api");
    assert!(
        poor_notices.iter().any(|(n, w)| {
            n == "eve_public.cosmetic.market.transaction.api.TransactionFailedNotice"
                && bytes(&sub(w, 1), 1) == failed_tx_uuid
        }),
        "Underfunded buyer must receive TransactionFailedNotice"
    );

    // 4b. Purchase with quantity > MAX_LISTING_QUANTITY gets 400 without modifying balances.
    let mut ovf_req = Vec::new();
    uuid_field(1, &listing_uuid, &mut ovf_req);
    encode_varint_field(2, 10_001, &mut ovf_req);
    encode_string_field(3, BUYER_FEES_VERSION, &mut ovf_req);
    let rsp_ovf = quasar_rpc(&ctx, &mut buyer, "eve_public.cosmetic.market.skin.listing.api.InitiatePurchaseListingRequest", ovf_req).await;
    assert_eq!(rsp_ovf.status_code, STATUS_BAD_REQUEST);
    ctx.db.with_snapshot(|s| {
        assert_eq!(s.characters[&(buyer_cid as i64)].balance, IskCents::from_isk(5_000_000));
        assert_eq!(s.characters[&(buyer_cid as i64)].loyalty_points[&(PARAGON_CORP_ID as i64)], 50_000);
    });

    // 4c. Buyer with insufficient EverMarks gets 412 and TransactionFailedNotice; balance untouched.
    let no_em_cid = 90_000_424u32;
    seed_char(&ctx, no_em_cid as i64, CORP_B, 5_000_000, 0);
    ctx.db.with_mut_snapshot(|s| {
        s.characters.get_mut(&(no_em_cid as i64)).unwrap().loyalty_points.remove(&(PARAGON_CORP_ID as i64));
    });
    let mut no_em_buyer = player(72_006, no_em_cid, CORP_B);
    let mut no_em_rx = subscribe(&ctx, &mut no_em_buyer).await;
    let mut no_em_req = Vec::new();
    uuid_field(1, &listing_uuid, &mut no_em_req);
    encode_varint_field(2, 1, &mut no_em_req);
    encode_string_field(3, BUYER_FEES_VERSION, &mut no_em_req);
    let rsp_no_em = quasar_rpc(&ctx, &mut no_em_buyer, "eve_public.cosmetic.market.skin.listing.api.InitiatePurchaseListingRequest", no_em_req).await;
    assert_eq!(rsp_no_em.status_code, STATUS_PRECONDITION_FAILED);
    assert_eq!(ctx.db.with_snapshot(|s| s.characters[&(no_em_cid as i64)].balance), IskCents::from_isk(5_000_000));
    assert!(drain(&mut no_em_rx, "eve_public.cosmetic.market.transaction.api").iter().any(|(n, _)| n == "eve_public.cosmetic.market.transaction.api.TransactionFailedNotice"));

    // 5. Funded buyer purchases 2 units -> TransactionSucceededNotice, seller credited net of 20% tax, 1 unit left.
    let rsp_ok_buy = quasar_rpc(
        &ctx,
        &mut buyer,
        "eve_public.cosmetic.market.skin.listing.api.InitiatePurchaseListingRequest",
        buy_req,
    )
    .await;
    assert_eq!(rsp_ok_buy.status_code, STATUS_OK);
    let ok_tx_uuid = bytes(&sub(&decode(&rsp_ok_buy), 1), 1);
    let buyer_notices = drain(&mut buyer_rx, "eve_public.cosmetic.market.transaction.api");
    assert!(
        buyer_notices.iter().any(|(n, w)| {
            n == "eve_public.cosmetic.market.transaction.api.TransactionSucceededNotice"
                && bytes(&sub(w, 1), 1) == ok_tx_uuid
        }),
        "Funded buyer must receive TransactionSucceededNotice"
    );
    ctx.db.with_snapshot(|s| {
        assert_eq!(
            s.characters[&(buyer_cid as i64)].balance,
            IskCents::from_isk(4_000_000)
        );
        let expected_em_fee = 1_000 * 2; // tier 1 custom SKINR design in self.skinr.data.skins
        assert_eq!(
            s.characters[&(buyer_cid as i64)].loyalty_points[&(PARAGON_CORP_ID as i64)],
            50_000 - expected_em_fee
        );
        // Seller had 1,000,000 - 100,000 broker fee + 800,000 (80% of 1,000,000) = 1,700,000 ISK.
        assert_eq!(
            s.characters[&(seller_cid as i64)].balance,
            IskCents::from_isk(1_700_000)
        );
    });

    // 6. Non-seller cannot cancel (403); seller cancels remaining 1 unit -> GetRequest returns 404.
    let rsp_forbid_cancel = quasar_rpc(
        &ctx,
        &mut buyer,
        "eve_public.cosmetic.market.skin.listing.api.CancelListingRequest",
        get_req.clone(),
    )
    .await;
    assert_eq!(rsp_forbid_cancel.status_code, STATUS_FORBIDDEN);

    let rsp_cancel = quasar_rpc(
        &ctx,
        &mut seller,
        "eve_public.cosmetic.market.skin.listing.api.CancelListingRequest",
        get_req.clone(),
    )
    .await;
    assert_eq!(rsp_cancel.status_code, STATUS_OK);
    let rsp_after_cancel = quasar_rpc(
        &ctx,
        &mut buyer,
        "eve_public.cosmetic.market.skin.listing.api.GetRequest",
        get_req,
    )
    .await;
    assert_eq!(rsp_after_cancel.status_code, STATUS_NOT_FOUND);

    // 7. Admin SetExpirationRequest: non-admin rejected (403), GM expires listing.
    let _ = quasar_rpc(
        &ctx,
        &mut seller,
        "eve_public.cosmetic.market.skin.listing.api.InitiateCreateListingRequest",
        build_create_req("feedface00112233", 1),
    )
    .await;
    let rsp_all = quasar_rpc(
        &ctx,
        &mut buyer,
        "eve_public.cosmetic.market.skin.listing.api.GetAllRequest",
        Vec::new(),
    )
    .await;
    let second_uuid = bytes(&sub(&sub(&decode(&rsp_all), 1), 1), 1);
    let mut exp_req = Vec::new();
    uuid_field(1, &second_uuid, &mut exp_req);
    let mut ts = Vec::new();
    encode_varint_field(1, ctx.now_unix().saturating_sub(60), &mut ts);
    encode_bytes_field(2, &ts, &mut exp_req);

    let rsp_exp_forbid = quasar_rpc(
        &ctx,
        &mut seller,
        "eve_public.cosmetic.market.skin.listing.api.admin.SetExpirationRequest",
        exp_req.clone(),
    )
    .await;
    assert_eq!(rsp_exp_forbid.status_code, STATUS_FORBIDDEN);

    let rsp_exp_gm = quasar_rpc(
        &ctx,
        &mut gm,
        "eve_public.cosmetic.market.skin.listing.api.admin.SetExpirationRequest",
        exp_req,
    )
    .await;
    assert_eq!(rsp_exp_gm.status_code, STATUS_OK);

    let rsp_all_after_exp = quasar_rpc(
        &ctx,
        &mut buyer,
        "eve_public.cosmetic.market.skin.listing.api.GetAllRequest",
        Vec::new(),
    )
    .await;
    assert!(
        decode(&rsp_all_after_exp).fields.is_empty(),
        "Expired listing must not be returned by GetAllRequest"
    );
    ctx.db.with_snapshot(|s| {
        assert!(s.quasar_state_blobs.contains_key("quasar_cosmetics_market"));
    });
}

#[tokio::test]
async fn break_plastic_wrap_requires_docked_at_wrap_station_and_combat_rng_seeded() {
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let issuer_cid = 90_000_001i64;
    let courier_cid = 90_000_431i64;
    seed_char(&ctx, issuer_cid, CORP_A, 10_000_000, 0);
    seed_char(&ctx, courier_cid, CORP_B, 10_000_000, 0);
    let cargo_item_id = 990_500_010i64;
    ctx.db.with_mut_snapshot(|s| {
        s.items.insert(
            cargo_item_id,
            InventoryItemRecord {
                item_id: cargo_item_id,
                type_id: 34,
                owner_id: issuer_cid,
                location_id: 60_003_760,
                flag: InventoryFlag::Hangar,
                quantity: 500,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });

    let mut s_issuer = player(73_001, issuer_cid as u32, CORP_A);
    s_issuer.station_id = Some(60_003_760);
    let mut s_courier = player(73_002, courier_cid as u32, CORP_B);
    s_courier.station_id = Some(60_003_760);
    let empty_kw = HashMap::new();

    let created = ctx
        .dispatch_rpc(
            &mut s_issuer,
            "contractProxy",
            None,
            "CreateContract",
            &[
                PyRep::Integer(3), // Courier
                PyRep::Integer(0),
                PyRep::Integer(0),
                PyRep::Integer(1440),
                PyRep::Integer(1),
                PyRep::Integer(60_003_760),
                PyRep::Integer(60_003_760),
                PyRep::Real(0.0),
                PyRep::Real(10_000.0),
                PyRep::Real(50_000.0),
                PyRep::String("OF-16 Break Wrap Docking Check".into()),
                PyRep::String("".into()),
                PyRep::List(vec![PyRep::Integer(cargo_item_id)]),
            ],
            &empty_kw,
            None,
        )
        .await
        .expect("CreateContract");
    let PyRep::KeyVal(ckv) = created else {
        panic!("Expected KeyVal");
    };
    let contract_id = ckv.get("contractID").and_then(|v| v.as_i64()).unwrap();

    let accepted = ctx
        .dispatch_rpc(
            &mut s_courier,
            "contractProxy",
            None,
            "AcceptContract",
            &[PyRep::Integer(contract_id)],
            &empty_kw,
            None,
        )
        .await
        .expect("AcceptContract");
    let PyRep::KeyVal(akv) = accepted else {
        panic!("Expected KeyVal");
    };
    let wrap_id = akv.get("courierPackageID").and_then(|v| v.as_i64()).unwrap();

    // OF-16: Undocked caller cannot break plastic wrap in a station hangar.
    ctx.db.with_mut_snapshot(|s| {
        s.characters.get_mut(&courier_cid).unwrap().station_id = None;
    });
    s_courier.station_id = None;
    s_courier.structure_id = None;
    let err_undocked = ctx
        .dispatch_rpc(
            &mut s_courier,
            "contractProxy",
            None,
            "BreakPlasticWrap",
            &[PyRep::Integer(wrap_id)],
            &empty_kw,
            None,
        )
        .await
        .expect_err("Undocked caller must not break station plastic wrap");
    assert_eq!(err_undocked.code, "CustomInfo");

    // OF-16: Caller docked at a different station (60_003_761) is also rejected.
    ctx.db.with_mut_snapshot(|s| {
        s.characters.get_mut(&courier_cid).unwrap().station_id = Some(60_003_761);
    });
    s_courier.station_id = Some(60_003_761);
    let err_wrong_station = ctx
        .dispatch_rpc(
            &mut s_courier,
            "contractProxy",
            None,
            "BreakPlasticWrap",
            &[PyRep::Integer(wrap_id)],
            &empty_kw,
            None,
        )
        .await
        .expect_err("Caller at wrong station must not break plastic wrap");
    assert_eq!(err_wrong_station.code, "CustomInfo");

    // OF-16: Caller docked at the wrap's station (60_003_760) succeeds and unwraps the inner item.
    ctx.db.with_mut_snapshot(|s| {
        s.characters.get_mut(&courier_cid).unwrap().station_id = Some(60_003_760);
    });
    s_courier.station_id = Some(60_003_760);
    ctx.dispatch_rpc(
        &mut s_courier,
        "contractProxy",
        None,
        "BreakPlasticWrap",
        &[PyRep::Integer(wrap_id)],
        &empty_kw,
        None,
    )
    .await
    .expect("Docked caller at wrap station breaks wrap");
    ctx.db.with_snapshot(|s| {
        assert!(!s.items.contains_key(&wrap_id));
        let inner = &s.items[&cargo_item_id];
        assert_eq!(inner.location_id, 60_003_760);
        assert_eq!(inner.flag, InventoryFlag::Hangar);
    });

    // OF-31: Distinct ship/NPC combat states receive distinct, non-default rng_state seeds.
    let ship_a = ctx.build_ship_combat_state(issuer_cid, 100_000_001, 587);
    let ship_b = ctx.build_ship_combat_state(issuer_cid, 100_000_002, 587);
    let npc_a = ctx.build_type_combat_state(587);
    let npc_b = ctx.build_type_combat_state(587);
    let default_seed = eve_destiny::CombatState::default().rng_state;
    assert_ne!(ship_a.rng_state, 0);
    assert_ne!(ship_a.rng_state, default_seed);
    assert_ne!(ship_a.rng_state, ship_b.rng_state);
    assert_ne!(npc_a.rng_state, 0);
    assert_ne!(npc_a.rng_state, default_seed);
    assert_ne!(npc_a.rng_state, npc_b.rng_state);
}
