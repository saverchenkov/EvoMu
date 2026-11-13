//! End-to-end Quasar RPC regression tests for Round-6 P1 fixes:
//! - `#403`: Unified PLEX Vault & SKINR / Paragon Hub / Deposit / Withdraw PLEX flows + `BalanceChangedNotice`
//! - `#404`: 3rd-party SKIN entitlement redemption materialization into `SkinrCosmeticsState`
//!   (`GetOwnedRequest`, `GetRequest`, `ActivateRequest`, `InitiateCreateListingRequest` without double-counting)

#![forbid(unsafe_code)]

use crate::quasar_gateway::{PublicGatewayRouter, STATUS_BAD_REQUEST, STATUS_OK};
use eve_db::WorldRepository;
use eve_machonet::{service_roles, SessionState};
use eve_proto::pb;
use eve_proto::quasar::{
    AuthoritativeContext, EvePublicRequest, EvePublicResponse, GoogleAny, GoogleDuration,
    GoogleTimestamp, QuasarDescriptorRegistry, WireMessage, WireValue, TYPE_GOOGLEAPIS_PREFIX,
};
use eve_sde::SdeRegistry;
use std::sync::Arc;

fn new_r6_harness() -> (PublicGatewayRouter, Arc<WorldRepository>) {
    let db = Arc::new(WorldRepository::with_demo_seed());
    let mut seed_skinr = super::quasar_cosmetics_skinr::SkinrCosmeticsData::default();
    for hex in [
        "aabbccdd11223344556677889900aabb",
        "deadbeefcafebabe0102030405060708",
    ] {
        seed_skinr.skins.insert(
            hex.to_string(),
            super::quasar_cosmetics_skinr::SkinDesignRecord {
                skin_hex: hex.to_string(),
                name: "TestSkin".into(),
                line_name: "TestLine".into(),
                creator_id: 90_000_002,
                ship_type_id: 587,
                pattern_blend_mode: 1,
                slots: Vec::new(),
                tier_level: 1,
                first_sequenced_unix: None,
            },
        );
    }
    db.with_mut_snapshot(|s| {
        s.quasar_state_blobs.insert(
            super::quasar_cosmetics_skinr::SKINR_BLOB_KEY.into(),
            serde_json::to_string(&seed_skinr).unwrap(),
        );
    });
    let sde = Arc::new(SdeRegistry::with_canonical_seed());
    let router = PublicGatewayRouter::new(
        QuasarDescriptorRegistry::open_default_or_empty(),
        Arc::clone(&db),
        sde,
    );
    (router, db)
}

fn make_sess(uid: u64, cid: i64, role: u64) -> SessionState {
    let mut s = SessionState::new(uid);
    s.user_id = Some(uid);
    s.character_id = Some(cid);
    s.corporation_id = Some(1_000_170);
    s.solar_system_id = Some(30_000_142);
    s.role = role;
    s
}

fn call_rpc(
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
        correlation_uuid: vec![9; 16],
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

fn wire_u64(val: &WireValue) -> Option<u64> {
    match val {
        WireValue::Varint(u) => Some(*u),
        _ => None,
    }
}

fn query_plex_vault_balance(router: &PublicGatewayRouter, sess: &SessionState, now: u64) -> i64 {
    let rsp = call_rpc(
        router,
        sess,
        "eve_public.plex.vault.api.BalanceRequest",
        Vec::new(),
        now,
    );
    assert_eq!(rsp.status_code, STATUS_OK);
    let wm = WireMessage::decode(&rsp.payload.unwrap().value).unwrap();
    let curr = WireMessage::decode(wm.fields[0].value.as_bytes().unwrap()).unwrap();
    let zz = wire_u64(&curr.fields[0].value).unwrap();
    (((zz >> 1) as i64) ^ (-((zz & 1) as i64))) / 100
}

fn dispatch_with_notices(
    router: &PublicGatewayRouter,
    sess: &SessionState,
    type_name: &str,
    value: Vec<u8>,
    now: u64,
) -> (Result<GoogleAny, (u32, String)>, Vec<String>) {
    let ctx = AuthoritativeContext::for_session_at(
        sess.user_id.map(|u| u as i32),
        sess.character_id.map(|c| c as u32),
        sess.corporation_id.map(|c| c as u32),
        0,
        "tranquility".to_string(),
        b"evomu-quasar-gw1".to_vec(),
        now,
    )
    .with_service_roles(sess.role);
    let any = GoogleAny {
        type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{type_name}"),
        value,
    };
    let norm = any.normalized_type_name();
    let handler = router
        .handlers
        .read()
        .unwrap()
        .get(norm)
        .cloned()
        .expect("registered handler");
    let dyn_msg = router
        .descriptors
        .decode_dynamic_or_wire(norm, &any.value)
        .expect("decode request");
    let res = handler(&ctx, &any, &dyn_msg);
    let mut list = router.pending_notices.write().unwrap();
    let mut notices = list
        .iter()
        .map(|(_, n)| n.type_url.clone())
        .collect::<Vec<_>>();
    list.clear();
    for (_, payload, _) in crate::services::quasar_social_chat::drain_local_notices() {
        notices.push(payload.type_url);
    }
    (res, notices)
}

#[test]
fn regression_403_unified_plex_vault_across_deposit_paragon_hub_and_withdraw() {
    let (router, db) = new_r6_harness();
    let now = 1_760_001_000u64;
    let buyer_uid = 1u64;
    let buyer_cid = 90_000_001i64;
    let seller_uid = 2u64;
    let seller_cid = 90_000_002i64;

    let buyer_admin = make_sess(buyer_uid, buyer_cid, service_roles::ROLE_GM_MASK);
    let buyer_player = make_sess(buyer_uid, buyer_cid, service_roles::ROLE_PLAYER_DEFAULT);
    let seller_player = make_sess(seller_uid, seller_cid, service_roles::ROLE_PLAYER_DEFAULT);
    let seller_gm = make_sess(seller_uid, seller_cid, service_roles::ROLE_GM_MASK);

    // 1. Seeded PLEX Vault has 500 PLEX; deposit +250 PLEX via DepositRequest -> 750 PLEX
    assert_eq!(query_plex_vault_balance(&router, &buyer_player, now), 500);
    let dep_req = pb::message("eve.plex.vault.assetholding.operation.api.DepositRequest")
        .set("plex.total_in_cents", 250i64 * 100)
        .must_encode();
    let (r_dep, dep_notices) = dispatch_with_notices(
        &router,
        &buyer_admin,
        "eve.plex.vault.assetholding.operation.api.DepositRequest",
        dep_req,
        now,
    );
    assert!(r_dep.is_ok());
    assert_eq!(query_plex_vault_balance(&router, &buyer_player, now), 750);
    assert!(dep_notices
        .iter()
        .any(|t| t.ends_with("BalanceChangedNotice")));
    // Verify #893: encode_plex_balance_changed_notice encodes V24.01 eve_public.plex.vault.transaction.Attributes fields 1, 2, 3
    let raw_notice = super::quasar_payment_store_types::encode_plex_balance_changed_notice(
        buyer_uid,
        1,
        250,
        750,
        "Deposit",
        now,
    );
    let raw_notice_wm = WireMessage::decode(&raw_notice.value).unwrap();
    let raw_attr_wm = WireMessage::decode(
        raw_notice_wm
            .fields
            .iter()
            .find(|f| f.field_number == 2)
            .unwrap()
            .value
            .as_bytes()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        raw_attr_wm
            .fields
            .iter()
            .map(|f| f.field_number)
            .collect::<Vec<_>>(),
        vec![1, 2, 3],
        "#893: BalanceChangedNotice.attributes must have fields 1 (timestamp), 2 (amount_transferred), 3 (resulting_balance)"
    );

    // 2. Grant seller an unactivated 3rd-party SKIN license & buyer EverMarks for tax
    let skin_hex = "aabbccdd11223344556677889900aabb".to_string();
    db.with_mut_snapshot(|s| {
        if let Some(b) = s.characters.get_mut(&buyer_cid) {
            b.loyalty_points.insert(
                super::quasar_cosmetics_market::PARAGON_CORP_ID as i64,
                5_000,
            );
        }
    });
    let grant_req = pb::message("eve_public.cosmetic.ship.skin.thirdparty.license.api.admin.GrantRequest")
        .set("skin.hex", skin_hex.as_str())
        .set("unactivated", 1u64)
        .must_encode();
    let (r_grant, _) = dispatch_with_notices(
        &router,
        &seller_gm,
        "eve_public.cosmetic.ship.skin.thirdparty.license.api.admin.GrantRequest",
        grant_req,
        now,
    );
    assert!(r_grant.is_ok());

    // 3. Seller lists 3rd-party SKIN for 200 PLEX on Paragon Hub
    let create_listing = pb::message("eve_public.cosmetic.market.skin.listing.api.InitiateCreateListingRequest")
        .set("price_plex.total_in_cents", 200i64 * 100)
        .set("skin.character.sequential", seller_cid as u32)
        .set("skin.skin.hex", skin_hex.as_str())
        .set("duration", GoogleDuration { seconds: 86_400, nanos: 0 })
        .set("quantity", 1u32)
        .must_encode();
    let r_list = call_rpc(
        &router,
        &seller_player,
        "eve_public.cosmetic.market.skin.listing.api.InitiateCreateListingRequest",
        create_listing,
        now,
    );
    assert_eq!(r_list.status_code, STATUS_OK);

    // Fetch created listing UUID
    let r_all = call_rpc(
        &router,
        &buyer_player,
        "eve_public.cosmetic.market.skin.listing.api.GetAllRequest",
        Vec::new(),
        now,
    );
    assert_eq!(r_all.status_code, STATUS_OK);
    let wm_all = WireMessage::decode(&r_all.payload.unwrap().value).unwrap();
    let entry_wm = WireMessage::decode(wm_all.fields[0].value.as_bytes().unwrap()).unwrap();
    let id_wm = WireMessage::decode(entry_wm.fields[0].value.as_bytes().unwrap()).unwrap();
    let listing_uuid = id_wm.fields[0].value.as_bytes().unwrap().to_vec();

    // 4. Buyer purchases listing using deposited PLEX Vault funds -> buyer -200 PLEX, seller credited net PLEX
    let seller_before = query_plex_vault_balance(&router, &seller_player, now);
    let buy_req = pb::message("eve_public.cosmetic.market.skin.listing.api.InitiatePurchaseListingRequest")
        .set("listing.uuid", listing_uuid)
        .set("quantity", 1u32)
        .set("buyer_fees_version", super::quasar_cosmetics_market::BUYER_FEES_VERSION)
        .must_encode();
    let (r_buy, buy_notices) = dispatch_with_notices(
        &router,
        &buyer_player,
        "eve_public.cosmetic.market.skin.listing.api.InitiatePurchaseListingRequest",
        buy_req,
        now,
    );
    assert!(r_buy.is_ok());

    assert_eq!(query_plex_vault_balance(&router, &buyer_player, now), 550);
    let seller_after = query_plex_vault_balance(&router, &seller_player, now);
    assert!(
        seller_after > seller_before,
        "seller PLEX Vault should increase after Paragon Hub sale"
    );
    assert_eq!(
        buy_notices
            .iter()
            .filter(|t| t.ends_with("BalanceChangedNotice"))
            .count(),
        2
    );

    // 5. Seller withdraws credited PLEX proceeds via WithdrawRequest -> no split-brain shortfall
    let proceeds = seller_after - seller_before;
    let w_req = pb::message("eve.plex.vault.assetholding.operation.api.WithdrawRequest")
        .set("plex.total_in_cents", proceeds * 100)
        .must_encode();
    let (r_withdraw, withdraw_notices) = dispatch_with_notices(
        &router,
        &seller_gm,
        "eve.plex.vault.assetholding.operation.api.WithdrawRequest",
        w_req,
        now,
    );
    assert!(r_withdraw.is_ok());
    assert!(withdraw_notices
        .iter()
        .any(|t| t.ends_with("BalanceChangedNotice")));
    assert_eq!(
        query_plex_vault_balance(&router, &seller_player, now),
        seller_before
    );
}

#[test]
fn regression_404_thirdparty_skin_entitlement_redeem_materializes_without_double_count() {
    let (router, _db) = new_r6_harness();
    let now = 1_760_002_000u64;
    let cid = 90_000_001i64;
    let uid = 1u64;
    let gm = make_sess(uid, cid, service_roles::ROLE_GM_MASK);
    let player = make_sess(uid, cid, service_roles::ROLE_PLAYER_DEFAULT);
    let skin_hex = "deadbeefcafebabe0102030405060708".to_string();

    // 1. Create a 3rd-party SKIN license asset (unit.unit_ship_skin_license.skin.hex)
    let create_asset = pb::message("eve.assetholding.asset.api.CreateRequest")
        .set("unit.unit_ship_skin_license.skin.hex", skin_hex.as_str())
        .must_encode();

    let (r_create, _) = dispatch_with_notices(
        &router,
        &gm,
        "eve.assetholding.asset.api.CreateRequest",
        create_asset,
        now,
    );
    let wm_create = WireMessage::decode(&r_create.unwrap().value).unwrap();
    let asset_id_msg = wm_create.fields[0].value.as_bytes().unwrap().to_vec();

    // 2. Grant entitlement with total = 2 units to character via DirectDeliveryRequest
    let grant_ent = pb::message("eve.assetholding.entitlement.api.DirectDeliveryRequest")
        .add_with("entitlements", |ent| {
            ent.set("entitlement.holder.character.sequential", cid as u32)
                .set_raw_message("entitlement.asset", &asset_id_msg)
                .set("quantity", 2u64)
        })
        .must_encode();
    let (r_grant_ent, _) = dispatch_with_notices(
        &router,
        &gm,
        "eve.assetholding.entitlement.api.DirectDeliveryRequest",
        grant_ent,
        now,
    );
    assert!(r_grant_ent.is_ok());

    // 3. Player redeems entitlement via eve_public.assetholding.entitlement.api.RedeemRequest
    let redeem_req = pb::message("eve_public.assetholding.entitlement.api.RedeemRequest")
        .set("entitlement.holder.character.sequential", cid as u32)
        .set_raw_message("entitlement.asset", &asset_id_msg)
        .must_encode();
    assert_eq!(
        call_rpc(
            &router,
            &player,
            "eve_public.assetholding.entitlement.api.RedeemRequest",
            redeem_req,
            now,
        )
        .status_code,
        STATUS_OK
    );

    // 4. Query GetRequest -> unactivated must be exact 2 (NOT 4 double-counted!)
    let get_req = pb::message("eve_public.cosmetic.ship.skin.thirdparty.license.api.GetRequest")
        .set("license.character.sequential", cid as u32)
        .set("license.skin.hex", skin_hex.as_str())
        .must_encode();

    let r_get = call_rpc(
        &router,
        &player,
        "eve_public.cosmetic.ship.skin.thirdparty.license.api.GetRequest",
        get_req.clone(),
        now,
    );
    assert_eq!(r_get.status_code, STATUS_OK);
    let wm_get = WireMessage::decode(&r_get.payload.unwrap().value).unwrap();
    let attrs_wm = WireMessage::decode(wm_get.fields[0].value.as_bytes().unwrap()).unwrap();
    let unactivated = attrs_wm
        .fields
        .iter()
        .find(|f| f.field_number == 2)
        .and_then(|f| wire_u64(&f.value))
        .unwrap_or(0);
    assert_eq!(unactivated, 2, "redeemed 3rd-party SKIN license must report exact unactivated count without double counting");

    // 5. Activate 1 unit via ActivateRequest -> must succeed (not STATUS_NOT_FOUND!) and leave unactivated == 1
    let act_req = pb::message("eve_public.cosmetic.ship.skin.thirdparty.license.api.ActivateRequest")
        .set("license.character.sequential", cid as u32)
        .set("license.skin.hex", skin_hex.as_str())
        .must_encode();
    let r_act = call_rpc(
        &router,
        &player,
        "eve_public.cosmetic.ship.skin.thirdparty.license.api.ActivateRequest",
        act_req,
        now,
    );
    assert_eq!(r_act.status_code, STATUS_OK);

    let r_get_after = call_rpc(
        &router,
        &player,
        "eve_public.cosmetic.ship.skin.thirdparty.license.api.GetRequest",
        get_req,
        now,
    );
    assert_eq!(r_get_after.status_code, STATUS_OK);
    let wm_after = WireMessage::decode(&r_get_after.payload.unwrap().value).unwrap();
    let attrs_after = WireMessage::decode(wm_after.fields[0].value.as_bytes().unwrap()).unwrap();
    let unactivated_after = attrs_after
        .fields
        .iter()
        .find(|f| f.field_number == 2)
        .and_then(|f| wire_u64(&f.value))
        .unwrap_or(0);
    assert_eq!(unactivated_after, 1);

    // 6. Verify admin GetRequest without payload rejects malformed request
    assert_eq!(
        call_rpc(
            &router,
            &gm,
            "eve.assetholding.entitlement.api.GetRequest",
            Vec::new(),
            now,
        )
        .status_code,
        STATUS_BAD_REQUEST
    );
}
