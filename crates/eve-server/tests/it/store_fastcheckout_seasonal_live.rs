//! Live integration tests for `storeManager`, `FastCheckoutService`, and `seasonalLoginCampaignManager`.

use std::collections::HashMap;

use eve_db::InventoryFlag;
use eve_machonet::{service_roles, SessionState};
use eve_proto::quasar::{
    encode_bytes_field, encode_string_field, encode_varint_field, EvePublicRequest,
    EvePublicResponse, GoogleAny, GoogleTimestamp, QuasarMessage, TYPE_GOOGLEAPIS_PREFIX,
};
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::quasar_gateway::STATUS_OK;
use eve_server::router::ServerContext;

use crate::common;

const PILOT_1: i64 = 90_000_001;
const JITA_STATION: i64 = 60_003_760;
const PLEX_TYPE_ID: u32 = 44_992;
const SKIN_TYPE_ID: u32 = 34_399;
const SEASONAL_CAMPAIGN_ID: i64 = 2323;
const ERR_NO_CAMP: &str = "NoActiveCampaign";
const ERR_ALREADY_CLAIMED: &str = "SeasonalRewardAlreadyClaimedToday";

async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let kw = HashMap::new();
    ctx.dispatch_rpc(sess, service, None, method, args, &kw, None)
        .await
}

async fn slash(ctx: &ServerContext, sess: &mut SessionState, cmd: &str) -> PyRep {
    let prev_role = sess.role;
    sess.role |= service_roles::ROLE_GMH | service_roles::ROLE_GM_DEFAULT;
    let res = rpc(ctx, sess, "slash", "SlashCmd", &[PyRep::String(cmd.into())])
        .await
        .unwrap_or_else(|e| panic!("slash `{cmd}` failed: {e:?}"));
    sess.role = prev_role;
    res
}

async fn quasar_admin(
    ctx: &ServerContext,
    sess: &mut SessionState,
    type_name: &str,
    payload_bytes: Vec<u8>,
) -> EvePublicResponse {
    let prev_role = sess.role;
    sess.role |= service_roles::ROLE_GM_DEFAULT;
    let req = EvePublicRequest {
        issued: Some(GoogleTimestamp {
            seconds: ctx.now_unix() as i64,
            nanos: 0,
        }),
        correlation_uuid: b"store-fc-seasonal-01".to_vec(),
        payload: Some(GoogleAny {
            type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{type_name}"),
            value: payload_bytes,
        }),
        external_origin: "eve-client".into(),
        application_instance_uuid: b"app-store-fc-test".to_vec(),
        authoritative_context: None,
        trace_context: None,
    };
    let rep = rpc(
        ctx,
        sess,
        "publicGatewaySvc",
        "SendRequest",
        &[PyRep::Bytes(req.encode_proto())],
    )
    .await
    .expect("SendRequest");
    sess.role = prev_role;
    let PyRep::KeyVal(entries) = rep else {
        panic!("expected KeyVal response_proto, got {rep:?}");
    };
    for (k, v) in entries {
        if k == "response_proto" {
            if let PyRep::Bytes(b) = v {
                return EvePublicResponse::decode_proto(&b).expect("decode EvePublicResponse");
            }
        }
    }
    panic!("missing response_proto");
}

fn encode_seq_id(id: u64) -> Vec<u8> {
    let mut b = Vec::new();
    encode_varint_field(1, id, &mut b);
    b
}

fn parse_first_nested_varint(mut buf: &[u8]) -> u64 {
    // Field 1 (length-delimited) -> Field 1 (varint)
    while !buf.is_empty() {
        let mut tag = 0u64;
        let mut shift = 0;
        loop {
            let b = buf[0];
            buf = &buf[1..];
            tag |= ((b & 0x7f) as u64) << shift;
            if b & 0x80 == 0 {
                break;
            }
            shift += 7;
        }
        let field = (tag >> 3) as u32;
        let wire = (tag & 0x7) as u32;
        if wire == 2 {
            let mut len = 0u64;
            let mut s = 0;
            loop {
                let b = buf[0];
                buf = &buf[1..];
                len |= ((b & 0x7f) as u64) << s;
                if b & 0x80 == 0 {
                    break;
                }
                s += 7;
            }
            let sub = &buf[..len as usize];
            buf = &buf[len as usize..];
            if field == 1 {
                let v = parse_first_nested_varint(sub);
                if v > 0 {
                    return v;
                }
                if let Ok(s) = std::str::from_utf8(sub) {
                    if let Ok(parsed) = s.parse::<u64>() {
                        return parsed;
                    }
                }
            }
        } else if wire == 0 {
            let mut val = 0u64;
            let mut s = 0;
            loop {
                let b = buf[0];
                buf = &buf[1..];
                val |= ((b & 0x7f) as u64) << s;
                if b & 0x80 == 0 {
                    break;
                }
                s += 7;
            }
            if field == 1 {
                return val;
            }
        }
    }
    0
}

async fn create_store_category(ctx: &ServerContext, sess: &mut SessionState, name: &str) -> u64 {
    let (mut attrs, mut req) = (Vec::new(), Vec::new());
    encode_string_field(1, name, &mut attrs);
    encode_bytes_field(1, &attrs, &mut req);
    let resp = quasar_admin(ctx, sess, "eve.store.admin.api.CreateCategoryRequest", req).await;
    assert_eq!(resp.status_code, STATUS_OK);
    parse_first_nested_varint(&resp.payload.expect("category payload").value)
}

async fn create_store_product(
    ctx: &ServerContext,
    sess: &mut SessionState,
    name: &str,
    type_id: u32,
) -> u64 {
    let (mut item, mut attrs, mut req) = (Vec::new(), Vec::new(), Vec::new());
    encode_bytes_field(1, &encode_seq_id(u64::from(type_id)), &mut item);
    encode_varint_field(2, 1, &mut item);
    encode_string_field(1, name, &mut attrs);
    encode_bytes_field(2, &item, &mut attrs);
    encode_varint_field(1, 1, &mut req);
    encode_bytes_field(2, &attrs, &mut req);
    let resp = quasar_admin(ctx, sess, "eve.store.admin.api.CreateProductRequest", req).await;
    assert_eq!(resp.status_code, STATUS_OK);
    parse_first_nested_varint(&resp.payload.expect("product payload").value)
}

async fn create_store_offer(
    ctx: &ServerContext,
    sess: &mut SessionState,
    name: &str,
    price_plex: u64,
    category_id: u64,
    product_id: u64,
) -> u64 {
    let (mut plex_price, mut attrs, mut req) = (Vec::new(), Vec::new(), Vec::new());
    encode_varint_field(1, price_plex * 100, &mut plex_price);
    encode_string_field(1, name, &mut attrs);
    encode_bytes_field(5, &plex_price, &mut attrs);
    encode_bytes_field(17, &encode_seq_id(category_id), &mut attrs);
    let mut prod_msg = Vec::new();
    encode_bytes_field(1, &encode_seq_id(product_id), &mut prod_msg);
    encode_bytes_field(18, &prod_msg, &mut attrs);
    encode_bytes_field(1, &attrs, &mut req);
    let resp = quasar_admin(ctx, sess, "eve.store.admin.api.CreateOfferRequest", req).await;
    assert_eq!(resp.status_code, STATUS_OK);
    parse_first_nested_varint(&resp.payload.expect("offer payload").value)
}

#[tokio::test]
async fn test_store_manager_categories_and_products_live() {
    // parity: rpc:storeManager.get_categories
    // parity: rpc:storeManager.get_products
    eve_testkit::attest(&[
        "rpc:storeManager.get_categories",
        "rpc:storeManager.get_products",
    ]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let mut unauth = SessionState::new(96_101);
    assert_eq!(
        rpc(&ctx, &mut unauth, "storeManager", "get_categories", &[])
            .await
            .unwrap_err()
            .code,
        "Unauthorized"
    );
    assert_eq!(
        rpc(&ctx, &mut unauth, "storeManager", "get_products", &[])
            .await
            .unwrap_err()
            .code,
        "Unauthorized"
    );

    let mut sess = SessionState::new(96_102);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    rpc(
        &ctx,
        &mut sess,
        "charUnboundMgr",
        "SelectCharacterID",
        &[PyRep::Integer(PILOT_1)],
    )
    .await
    .expect("SelectCharacterID");

    let cat_id = create_store_category(&ctx, &mut sess, "Ship SKINs").await;
    assert!(cat_id > 0);
    let prod_id = create_store_product(&ctx, &mut sess, "Svipul SKIN", SKIN_TYPE_ID).await;
    assert!(prod_id > 0);

    let PyRep::List(cats) = rpc(&ctx, &mut sess, "storeManager", "get_categories", &[])
        .await
        .expect("get_categories")
    else {
        panic!("expected List from get_categories");
    };
    assert_eq!(cats.len(), 2, "root + subcategory expected");
    assert_eq!(cats[0].dict_get("id").and_then(|v| v.as_i64()), Some(cat_id as i64));
    assert_eq!(cats[0].dict_get("name").and_then(|v| v.as_str()), Some("Ship SKINs"));
    assert_eq!(cats[0].dict_get("parent"), Some(&PyRep::None));

    let PyRep::List(prods) = rpc(&ctx, &mut sess, "storeManager", "get_products", &[])
        .await
        .expect("get_products")
    else {
        panic!("expected List from get_products");
    };
    assert_eq!(prods.len(), 1);
    assert_eq!(prods[0].dict_get("id").and_then(|v| v.as_i64()), Some(prod_id as i64));
    assert_eq!(prods[0].dict_get("name").and_then(|v| v.as_str()), Some("Svipul SKIN"));
    assert_eq!(
        prods[0].dict_get("typeID").and_then(|v| v.as_i64()),
        Some(i64::from(SKIN_TYPE_ID))
    );
}

#[tokio::test]
async fn test_fast_checkout_offers_and_buy_live() {
    // parity: rpc:FastCheckoutService.GetOffersForUser
    // parity: rpc:FastCheckoutService.BuyOffer
    eve_testkit::attest(&[
        "rpc:FastCheckoutService.GetOffersForUser",
        "rpc:FastCheckoutService.BuyOffer",
    ]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let mut unauth = SessionState::new(96_201);
    assert_eq!(
        rpc(&ctx, &mut unauth, "FastCheckoutService", "GetOffersForUser", &[])
            .await
            .unwrap_err()
            .code,
        "Unauthorized"
    );

    let mut sess = SessionState::new(96_202);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    rpc(
        &ctx,
        &mut sess,
        "charUnboundMgr",
        "SelectCharacterID",
        &[PyRep::Integer(PILOT_1)],
    )
    .await
    .expect("SelectCharacterID");

    // Disabled by default
    assert_eq!(
        rpc(&ctx, &mut sess, "FastCheckoutService", "GetOffersForUser", &[])
            .await
            .expect("GetOffersForUser disabled"),
        PyRep::List(Vec::new())
    );
    assert_eq!(
        rpc(
            &ctx,
            &mut sess,
            "FastCheckoutService",
            "BuyOffer",
            &[PyRep::Integer(1001), PyRep::Integer(1), PyRep::String("demo-secret".into())],
        )
        .await
        .unwrap_err()
        .code,
        "FastCheckoutDisabled"
    );

    // Enable FastCheckout via GM slash command and create catalog offer
    assert_eq!(slash(&ctx, &mut sess, "/fastcheckout 1").await, PyRep::Bool(true));
    let cat_id = create_store_category(&ctx, &mut sess, "Ship SKINs").await;
    let prod_id = create_store_product(&ctx, &mut sess, "Svipul SKIN", SKIN_TYPE_ID).await;
    let offer_id = create_store_offer(&ctx, &mut sess, "Svipul SKIN Pack", 150, cat_id, prod_id).await;

    let offers_rep = rpc(&ctx, &mut sess, "FastCheckoutService", "GetOffersForUser", &[])
        .await
        .expect("GetOffersForUser enabled");
    let PyRep::List(offers_list) = offers_rep
        .dict_get("offers")
        .cloned()
        .expect("offers key in GetOffersForUser")
    else {
        panic!("expected List for offers");
    };
    assert_eq!(offers_list.len(), 1);
    assert_eq!(
        offers_list[0].dict_get("offerId").and_then(|v| v.as_i64()),
        Some(offer_id as i64)
    );
    assert_eq!(
        offers_list[0].dict_get("pricePlex").and_then(|v| v.as_i64()),
        Some(150)
    );

    // Negative checks: wrong password, unknown offer, insufficient PLEX
    assert_eq!(
        rpc(
            &ctx,
            &mut sess,
            "FastCheckoutService",
            "BuyOffer",
            &[
                PyRep::Integer(offer_id as i64),
                PyRep::Integer(1),
                PyRep::String("wrong-pass".into()),
            ],
        )
        .await
        .unwrap_err()
        .code,
        "InvalidPassword"
    );
    assert_eq!(
        rpc(
            &ctx,
            &mut sess,
            "FastCheckoutService",
            "BuyOffer",
            &[
                PyRep::Integer(999_999),
                PyRep::Integer(1),
                PyRep::String("demo-secret".into()),
            ],
        )
        .await
        .unwrap_err()
        .code,
        "OfferNotFound"
    );
    assert_eq!(
        rpc(
            &ctx,
            &mut sess,
            "FastCheckoutService",
            "BuyOffer",
            &[
                PyRep::Integer(offer_id as i64),
                PyRep::Integer(10),
                PyRep::String("demo-secret".into()),
            ],
        )
        .await
        .unwrap_err()
        .code,
        "UserError"
    );

    // Provision 200 PLEX and deposit into PLEX vault
    let plex_kv = slash(&ctx, &mut sess, &format!("/giveitem {PLEX_TYPE_ID} 200")).await;
    let plex_item_id = plex_kv
        .dict_get("itemID")
        .and_then(|v| v.as_i64())
        .expect("plex itemID");
    rpc(
        &ctx,
        &mut sess,
        "invbroker",
        "DepositPlexToVault",
        &[
            PyRep::Integer(JITA_STATION),
            PyRep::Integer(plex_item_id),
            PyRep::Integer(200),
        ],
    )
    .await
    .expect("DepositPlexToVault");

    let buy_rep = rpc(
        &ctx,
        &mut sess,
        "FastCheckoutService",
        "BuyOffer",
        &[
            PyRep::Integer(offer_id as i64),
            PyRep::Integer(1),
            PyRep::String("demo-secret".into()),
        ],
    )
    .await
    .expect("BuyOffer");
    assert_eq!(
        buy_rep.dict_get("status").and_then(|v| v.as_str()),
        Some("CONFIRMED")
    );
    assert_eq!(
        buy_rep.dict_get("offerID").and_then(|v| v.as_i64()),
        Some(offer_id as i64)
    );
    assert_eq!(
        buy_rep.dict_get("itemsDelivered").and_then(|v| v.as_i64()),
        Some(1)
    );

    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.plex_vault_balance(PILOT_1), 550);
        assert!(snap.items.values().any(|it| {
            it.owner_id == PILOT_1
                && it.type_id == SKIN_TYPE_ID
                && it.flag == InventoryFlag::Hangar
                && it.quantity == 1
        }));
    });
}

#[tokio::test]
async fn test_seasonal_login_campaign_lifecycle_live() {
    // parity: rpc:seasonalLoginCampaignManager.get_active_campaign
    // parity: rpc:seasonalLoginCampaignManager.get_claim_history
    // parity: rpc:seasonalLoginCampaignManager.claim_reward
    eve_testkit::attest(&[
        "rpc:seasonalLoginCampaignManager.get_active_campaign",
        "rpc:seasonalLoginCampaignManager.get_claim_history",
        "rpc:seasonalLoginCampaignManager.claim_reward",
    ]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let mut sess = SessionState::new(96_301);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    rpc(
        &ctx,
        &mut sess,
        "charUnboundMgr",
        "SelectCharacterID",
        &[PyRep::Integer(PILOT_1)],
    )
    .await
    .expect("SelectCharacterID");

    // No active campaign initially
    assert_eq!(
        rpc(&ctx, &mut sess, "seasonalLoginCampaignManager", "get_active_campaign", &[])
            .await
            .expect("inactive get_active_campaign"),
        PyRep::Tuple(vec![PyRep::None, PyRep::None, PyRep::None, PyRep::None])
    );
    let no_camp_err = rpc(&ctx, &mut sess, "seasonalLoginCampaignManager", "claim_reward", &[])
        .await
        .unwrap_err();
    assert_eq!(no_camp_err.code, ERR_NO_CAMP);

    // Schedule SDE seasonal campaign 2323 via GM slash command
    slash(
        &ctx,
        &mut sess,
        &format!("/campaign seasonal {SEASONAL_CAMPAIGN_ID} 30 11:00:00"),
    )
    .await;

    let PyRep::Tuple(active_pre) =
        rpc(&ctx, &mut sess, "seasonalLoginCampaignManager", "get_active_campaign", &[])
            .await
            .expect("get_active_campaign")
    else {
        panic!("expected Tuple from get_active_campaign");
    };
    assert_eq!(active_pre.len(), 4);
    assert_eq!(active_pre[0].as_i64(), Some(SEASONAL_CAMPAIGN_ID));
    assert_eq!(active_pre[2].as_str(), Some("11:00:00"));
    assert_eq!(active_pre[3].as_i64(), Some(30));

    let PyRep::Dict(hist_pre) =
        rpc(&ctx, &mut sess, "seasonalLoginCampaignManager", "get_claim_history", &[])
            .await
            .expect("get_claim_history pre")
    else {
        panic!("expected Dict from get_claim_history");
    };
    assert!(hist_pre.is_empty());

    // Claim day 0 reward
    let claim_res = rpc(&ctx, &mut sess, "seasonalLoginCampaignManager", "claim_reward", &[])
        .await
        .expect("claim_reward");
    assert_eq!(claim_res, PyRep::Bool(true));

    // Remaining rewards decremented and claim history updated
    let PyRep::Tuple(active_post) =
        rpc(&ctx, &mut sess, "seasonalLoginCampaignManager", "get_active_campaign", &[])
            .await
            .expect("get_active_campaign post")
    else {
        panic!("expected Tuple");
    };
    assert_eq!(active_post[3].as_i64(), Some(29));

    let PyRep::Dict(hist_post) =
        rpc(&ctx, &mut sess, "seasonalLoginCampaignManager", "get_claim_history", &[])
            .await
            .expect("get_claim_history post")
    else {
        panic!("expected Dict");
    };
    assert_eq!(hist_post.len(), 1);

    // Duplicate claim on same day rejected
    let dup_err = rpc(&ctx, &mut sess, "seasonalLoginCampaignManager", "claim_reward", &[])
        .await
        .unwrap_err();
    assert_eq!(dup_err.code, ERR_ALREADY_CLAIMED);
}
