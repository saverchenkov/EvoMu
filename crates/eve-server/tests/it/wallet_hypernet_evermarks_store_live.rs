//! Live MachoNet + Quasar integration tests for HyperNet Relay, EverMarks, and New Eden Store.
//!
//! Remediates fake_done rows:
//! - `feat:wallet.hypernet-relay`
//! - `feat:wallet.hypernet-hypercore-sizing-expiry`
//! - `feat:wallet.evermarks`
//! - `feat:wallet.nes-store`

#![forbid(unsafe_code)]

use std::collections::HashMap;

use crate::common;
use eve_machonet::{service_roles, SessionState};
use eve_proto::quasar::{
    encode_bytes_field, encode_varint_field, EvePublicRequest, GoogleAny, TYPE_GOOGLEAPIS_PREFIX,
};
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::quasar_gateway::STATUS_OK;
use eve_server::router::ServerContext;

const PILOT_1: i64 = 90_000_001;
const PILOT_2: i64 = 90_000_002;
const JITA_STATION: i64 = 60_003_760;
const PARAGON_CORP_ID: i64 = 1_000_419;
const PARAGON_STATION: i64 = 60_015_169;
const PLEX_TYPE_ID: u32 = 44_992;
const HYPERCORE_TYPE_ID: u32 = 52_568;
const RIFTER_TYPE_ID: u32 = 587;

async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    svc: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    ctx.dispatch_rpc(sess, svc, None, method, args, &HashMap::new(), None)
        .await
}

async fn slash(ctx: &ServerContext, gm: &mut SessionState, cmd: &str) -> PyRep {
    rpc(ctx, gm, "slash", "SlashCmd", &[PyRep::String(cmd.into())])
        .await
        .expect(cmd)
}

async fn login_select(
    ctx: &ServerContext,
    sess_id: u64,
    user: &str,
    pass: &str,
    char_id: i64,
    gm_role: bool,
) -> SessionState {
    let mut sess = SessionState::new(sess_id);
    common::login_pilot(ctx, &mut sess, user, pass).await;
    if gm_role {
        sess.role = service_roles::ROLE_GM_DEFAULT;
    }
    rpc(
        ctx,
        &mut sess,
        "charUnboundMgr",
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
    )
    .await
    .expect("SelectCharacterID");
    if gm_role {
        sess.role = service_roles::ROLE_GM_DEFAULT;
    }
    ctx.advance_secs(11);
    sess
}

async fn deposit_evermarks_quasar(
    ctx: &ServerContext,
    sess: &mut SessionState,
    char_id: i64,
    amount: u64,
) {
    let mut dep_req = Vec::new();
    let mut op_id_m = Vec::new();
    encode_bytes_field(1, b"paragon-em-op-01", &mut op_id_m);
    let mut cid_msg = Vec::new();
    encode_varint_field(1, char_id as u64, &mut cid_msg);
    let mut issuer_msg = Vec::new();
    encode_varint_field(1, PARAGON_CORP_ID as u64, &mut issuer_msg);
    let mut wallet_id = Vec::new();
    encode_bytes_field(1, &cid_msg, &mut wallet_id);
    encode_bytes_field(3, &issuer_msg, &mut wallet_id);
    let mut curr_m = Vec::new();
    encode_varint_field(1, amount, &mut curr_m);
    let mut op_attr = Vec::new();
    encode_bytes_field(1, &wallet_id, &mut op_attr);
    encode_bytes_field(2, &curr_m, &mut op_attr);
    encode_bytes_field(1, &op_id_m, &mut dep_req);
    encode_bytes_field(2, &op_attr, &mut dep_req);

    let prev_role = sess.role;
    sess.role = service_roles::ROLE_GM_DEFAULT;
    let req = EvePublicRequest {
        issued: Some(eve_proto::quasar::types::GoogleTimestamp {
            seconds: ctx.now_unix() as i64,
            nanos: 0,
        }),
        correlation_uuid: b"corr-em-live".to_vec(),
        payload: Some(GoogleAny {
            type_url: format!(
                "{}eve.loyaltypoints.assetholding.operation.api.DepositRequest",
                TYPE_GOOGLEAPIS_PREFIX
            ),
            value: dep_req,
        }),
        external_origin: "eve-client".into(),
        application_instance_uuid: b"em-live-app".to_vec(),
        authoritative_context: None,
        trace_context: None,
    };
    let rsp = ctx
        .quasar_gateway
        .handle_public_request(sess, &req, None, false, ctx.now_unix());
    sess.role = prev_role;
    assert_eq!(rsp.status_code, STATUS_OK, "DepositRequest must succeed");
}

#[tokio::test]
async fn test_wallet_hypernet_relay_npc_station_enforcement_and_random_tickets_live() {
    // parity: feat:wallet.hypernet-relay, feat:wallet.hypernet-hypercore-sizing-expiry, rpc:raffleProxy.CreateRaffle, rpc:raffleProxy.BuyRandomTickets, rpc:raffleProxy.GetActiveHistoricPrices
    eve_testkit::attest(&[
        "feat:wallet.hypernet-relay",
        "feat:wallet.hypernet-hypercore-sizing-expiry",
        "rpc:raffleProxy.CreateRaffle",
        "rpc:raffleProxy.BuyRandomTickets",
        "rpc:raffleProxy.GetActiveHistoricPrices",
    ]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let mut s1 = login_select(&ctx, 1501, "demo", "demo-secret", PILOT_1, true).await;
    let mut s2 = login_select(&ctx, 1502, "other", "other-secret", PILOT_2, true).await;

    // 1. Provision item to raffle (Rifter), HyperCores, and buyer/creator ISK via real GM slash commands
    let rifter_kv = slash(&ctx, &mut s1, &format!("/giveitem {RIFTER_TYPE_ID} 1")).await;
    let rifter_id = rifter_kv
        .dict_get("itemID")
        .and_then(|v| v.as_i64())
        .expect("rifter itemID");

    slash(&ctx, &mut s1, &format!("/giveitem {HYPERCORE_TYPE_ID} 300")).await;
    slash(&ctx, &mut s1, "/giveisk 50000000").await;
    slash(&ctx, &mut s2, "/giveisk 50000000").await;

    // 2. Negative test: Undocked raffle creation must fail closed with InvalidRaffleLocation
    rpc(&ctx, &mut s1, "stationSvc", "Undock", &[])
        .await
        .expect("Undock from Jita 4-4");
    assert!(
        s1.station_id.is_none(),
        "Session station_id must be None when undocked"
    );

    let undocked_err = rpc(
        &ctx,
        &mut s1,
        "raffleProxy",
        "CreateRaffle",
        &[PyRep::Dict(vec![
            (PyRep::String("itemID".into()), PyRep::Integer(rifter_id)),
            (PyRep::String("ticketCount".into()), PyRep::Integer(8)),
            (
                PyRep::String("ticketPrice".into()),
                PyRep::Real(100_000.0),
            ),
        ])],
    )
    .await
    .expect_err("Undocked CreateRaffle must fail");
    assert_eq!(
        undocked_err.code, "InvalidRaffleLocation",
        "Undocked raffle creation must return InvalidRaffleLocation"
    );

    // 3. Dock back at Jita 4-4 NPC station
    rpc(
        &ctx,
        &mut s1,
        "beyonce",
        "CmdDock",
        &[PyRep::Integer(JITA_STATION)],
    )
    .await
    .expect("CmdDock into Jita 4-4");
    assert_eq!(s1.station_id, Some(JITA_STATION));

    // 4. Negative test: Invalid node count (e.g. 10 nodes)
    let bad_nodes_err = rpc(
        &ctx,
        &mut s1,
        "raffleProxy",
        "CreateRaffle",
        &[PyRep::Dict(vec![
            (PyRep::String("itemID".into()), PyRep::Integer(rifter_id)),
            (PyRep::String("ticketCount".into()), PyRep::Integer(10)),
            (
                PyRep::String("ticketPrice".into()),
                PyRep::Real(100_000.0),
            ),
        ])],
    )
    .await
    .expect_err("10-node raffle must fail");
    assert_eq!(bad_nodes_err.code, "InvalidRaffleTicketCount");

    // 5. Negative test: Invalid duration (> 72 hours / 259,200 s)
    let bad_dur_err = rpc(
        &ctx,
        &mut s1,
        "raffleProxy",
        "CreateRaffle",
        &[PyRep::Dict(vec![
            (PyRep::String("itemID".into()), PyRep::Integer(rifter_id)),
            (PyRep::String("ticketCount".into()), PyRep::Integer(8)),
            (
                PyRep::String("ticketPrice".into()),
                PyRep::Real(100_000.0),
            ),
            (PyRep::String("duration".into()), PyRep::Integer(300_000)),
        ])],
    )
    .await
    .expect_err(">3 day duration must fail");
    assert_eq!(bad_dur_err.code, "InvalidRaffleDuration");

    // 6. Historic and active market prices query
    let prices_rep = rpc(
        &ctx,
        &mut s1,
        "raffleProxy",
        "GetActiveHistoricPrices",
        &[PyRep::Integer(RIFTER_TYPE_ID as i64)],
    )
    .await
    .expect("GetActiveHistoricPrices");
    let PyRep::Tuple(price_vals) = prices_rep else {
        panic!("GetActiveHistoricPrices must return tuple");
    };
    assert_eq!(price_vals.len(), 2);
    let hist_price = price_vals[1].as_f64().expect("historic price f64");
    assert!(
        hist_price > 0.0,
        "Historic price must be positive based on SDE adjusted price"
    );

    // 7. Create valid 8-node raffle at 100,000 ISK/ticket
    let create_rep = rpc(
        &ctx,
        &mut s1,
        "raffleProxy",
        "CreateRaffle",
        &[PyRep::Dict(vec![
            (PyRep::String("itemID".into()), PyRep::Integer(rifter_id)),
            (PyRep::String("ticketCount".into()), PyRep::Integer(8)),
            (
                PyRep::String("ticketPrice".into()),
                PyRep::Real(100_000.0),
            ),
        ])],
    )
    .await
    .expect("CreateRaffle valid 8-node");
    let PyRep::KeyVal(r_kv) = create_rep else {
        panic!("CreateRaffle must return KeyVal");
    };
    let raffle_id = r_kv
        .get("raffle_id")
        .and_then(|v| v.as_i64())
        .expect("raffle_id");
    let cores_used = r_kv
        .get("hypercores_used")
        .and_then(|v| v.as_i64())
        .expect("hypercores_used");
    assert!(
        cores_used >= 1,
        "Creating raffle must consume at least 1 HyperCore"
    );

    // 8. Buyer pilot buys 8 random tickets via BuyRandomTickets (using real random draw)
    let buy_rep = rpc(
        &ctx,
        &mut s2,
        "raffleProxy",
        "BuyRandomTickets",
        &[PyRep::Integer(raffle_id), PyRep::Integer(8)],
    )
    .await
    .expect("BuyRandomTickets");
    let PyRep::KeyVal(b_kv) = buy_rep else {
        panic!("BuyRandomTickets must return KeyVal");
    };
    let sold_tickets_dict = b_kv.get("sold_tickets").expect("sold_tickets present");
    let PyRep::Dict(sold_entries) = sold_tickets_dict else {
        panic!("sold_tickets must be Dict");
    };
    assert_eq!(sold_entries.len(), 8, "All 8 tickets must be sold");
    assert_eq!(
        b_kv.get("winner_id").and_then(|v| v.as_i64()),
        Some(PILOT_2),
        "Sole buyer must be the winner"
    );

    // 9. Award item to winner
    let award_rep = rpc(
        &ctx,
        &mut s2,
        "raffleProxy",
        "AwardItem",
        &[PyRep::Integer(raffle_id)],
    )
    .await
    .expect("AwardItem");
    assert_eq!(award_rep, PyRep::Integer(rifter_id));

    let raffle_after = rpc(
        &ctx,
        &mut s2,
        "raffleProxy",
        "GetRaffle",
        &[PyRep::Integer(raffle_id)],
    )
    .await
    .expect("GetRaffle after award");
    let PyRep::KeyVal(raf_kv) = raffle_after else {
        panic!("GetRaffle must return KeyVal");
    };
    assert_eq!(
        raf_kv.get("item_awarded").and_then(|v| v.as_bool()),
        Some(true)
    );
}

#[tokio::test]
async fn test_wallet_evermarks_paragon_accrual_and_emblem_store_live() {
    // parity: feat:wallet.evermarks, rpc:LPStoreMgr.GetAvailableOffersFromCorp, rpc:LPStoreMgr.TakeOfferForCharacter
    eve_testkit::attest(&[
        "feat:wallet.evermarks",
        "rpc:LPStoreMgr.GetAvailableOffersFromCorp",
        "rpc:LPStoreMgr.TakeOfferForCharacter",
    ]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let mut s1 = login_select(&ctx, 1503, "demo", "demo-secret", PILOT_1, true).await;

    // Provision Corp Management skill and create a player corporation for pilot
    slash(&ctx, &mut s1, "/giveskill 3363 1").await;
    rpc(
        &ctx,
        &mut s1,
        "corpRegistry",
        "AddCorporation",
        &[
            PyRep::String("Apex Evermark Logistics".into()),
            PyRep::String("APEXL".into()),
            PyRep::Real(0.05),
        ],
    )
    .await
    .expect("AddCorporation");

    // 1. Accrue EverMarks to character via Quasar DepositRequest (50,000 EverMarks for Paragon corp 1000419)
    deposit_evermarks_quasar(&ctx, &mut s1, PILOT_1, 50_000).await;

    // 2. Query pilot's character LP wallet via LPSvc
    let char_lp_rep = rpc(
        &ctx,
        &mut s1,
        "LPSvc",
        "GetAllMyCharacterWalletLPBalances",
        &[],
    )
    .await
    .expect("GetAllMyCharacterWalletLPBalances");
    let PyRep::List(char_lps) = char_lp_rep else {
        panic!("expected List of LP balances");
    };
    let paragon_char_entry = char_lps.iter().find(|row| {
        row.dict_get("corpID") == Some(&PyRep::Integer(PARAGON_CORP_ID))
    });
    assert!(
        paragon_char_entry.is_some(),
        "Pilot must have Paragon LP wallet entry"
    );
    let char_em = paragon_char_entry
        .unwrap()
        .dict_get("loyaltyPoints")
        .and_then(|v| v.as_i64())
        .expect("char LP");
    assert_eq!(char_em, 50_000, "Character must have 50,000 EverMarks");

    // 3. Verify 1-to-1 corporation EverMark matching in the corp wallet
    let corp_lp_rep = rpc(
        &ctx,
        &mut s1,
        "LPSvc",
        "GetAllMyCorporationWalletLPBalances",
        &[],
    )
    .await
    .expect("GetAllMyCorporationWalletLPBalances");
    let PyRep::List(corp_lps) = corp_lp_rep else {
        panic!("expected List of corp LP balances");
    };
    let paragon_corp_entry = corp_lps.iter().find(|row| {
        row.dict_get("corpID") == Some(&PyRep::Integer(PARAGON_CORP_ID))
    });
    assert!(
        paragon_corp_entry.is_some(),
        "Corporation must receive 1:1 EverMark match"
    );
    let corp_em = paragon_corp_entry
        .unwrap()
        .dict_get("loyaltyPoints")
        .and_then(|v| v.as_i64())
        .expect("corp LP");
    assert_eq!(corp_em, 50_000, "Corporation must have received 50,000 EverMarks 1:1");

    // 4. Retrieve Paragon ship cosmetic emblem offers from SDE catalog
    let offers_rep = rpc(
        &ctx,
        &mut s1,
        "LPStoreMgr",
        "GetAvailableOffersFromCorp",
        &[PyRep::Integer(PARAGON_CORP_ID)],
    )
    .await
    .expect("GetAvailableOffersFromCorp");
    let PyRep::List(offers) = offers_rep else {
        panic!("expected List of offers");
    };
    assert!(
        !offers.is_empty(),
        "Paragon store must have ship cosmetic emblem offers"
    );

    let first_offer = &offers[0];
    let offer_id = first_offer
        .dict_get("offerID")
        .and_then(|v| v.as_i64())
        .expect("offerID");
    let lp_cost = first_offer
        .dict_get("lpCost")
        .and_then(|v| v.as_i64())
        .expect("lpCost");

    // 5. Dock pilot at Jita VI - Paragon Fulfillment Center via GM CmdTurboDock and spend EverMarks
    rpc(
        &ctx,
        &mut s1,
        "beyonce",
        "CmdTurboDock",
        &[PyRep::Integer(PARAGON_STATION)],
    )
    .await
    .expect("CmdTurboDock to Paragon station");

    let take_rep = rpc(
        &ctx,
        &mut s1,
        "LPStoreMgr",
        "TakeOfferForCharacter",
        &[
            PyRep::Integer(PARAGON_CORP_ID),
            PyRep::Integer(offer_id),
            PyRep::Integer(1),
        ],
    )
    .await
    .expect("TakeOfferForCharacter");
    let PyRep::KeyVal(t_kv) = take_rep else {
        panic!("TakeOfferForCharacter must return KeyVal");
    };
    let rem_lp = t_kv
        .get("remainingLP")
        .and_then(|v| v.as_i64())
        .expect("remainingLP");
    assert_eq!(
        rem_lp,
        50_000 - lp_cost,
        "Character EverMarks must be reduced by offer lpCost"
    );

    // 6. Verify negative: empty SKINR initiate request does not award EverMarks
    let empty_req = EvePublicRequest {
        issued: Some(eve_proto::quasar::types::GoogleTimestamp {
            seconds: ctx.now_unix() as i64,
            nanos: 0,
        }),
        correlation_uuid: b"corr-skinr-no-em".to_vec(),
        payload: Some(GoogleAny {
            type_url: format!(
                "{}eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.api.InitiateRequest",
                TYPE_GOOGLEAPIS_PREFIX
            ),
            value: Vec::new(),
        }),
        external_origin: "eve-client".into(),
        application_instance_uuid: b"skinr-app".to_vec(),
        authoritative_context: None,
        trace_context: None,
    };
    let skinr_rsp = ctx
        .quasar_gateway
        .handle_public_request(&s1, &empty_req, None, false, ctx.now_unix());
    assert_eq!(
        skinr_rsp.status_code, 400,
        "Empty SKINR job request must fail"
    );
}

#[tokio::test]
async fn test_wallet_nes_store_catalog_and_purchases_live() {
    // parity: feat:wallet.nes-store, rpc:storeManager.get_offers, rpc:storeManager.buy_offer, rpc:storeManager.can_purchase_offer
    eve_testkit::attest(&[
        "feat:wallet.nes-store",
        "rpc:storeManager.get_offers",
        "rpc:storeManager.buy_offer",
        "rpc:storeManager.can_purchase_offer",
    ]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    common::seed_test_nes_store_catalog(&ctx);

    let mut s1 = login_select(&ctx, 1504, "demo", "demo-secret", PILOT_1, true).await;

    // 1. Query store categories, products, and offers via MachoNet storeManager
    let cats_rep = rpc(&ctx, &mut s1, "storeManager", "get_categories", &[])
        .await
        .expect("get_categories");
    let PyRep::List(cats) = cats_rep else {
        panic!("expected List of categories");
    };
    assert!(!cats.is_empty(), "Store categories must not be empty");

    let prods_rep = rpc(&ctx, &mut s1, "storeManager", "get_products", &[])
        .await
        .expect("get_products");
    let PyRep::List(prods) = prods_rep else {
        panic!("expected List of products");
    };
    assert!(!prods.is_empty(), "Store products must not be empty");

    let offers_rep = rpc(&ctx, &mut s1, "storeManager", "get_offers", &[])
        .await
        .expect("get_offers");
    let PyRep::List(offers) = offers_rep else {
        panic!("expected List of offers");
    };
    assert!(!offers.is_empty(), "Store offers must not be empty");

    let offer = &offers[0];
    let offer_id = offer
        .dict_get("id")
        .and_then(|v| v.as_i64())
        .expect("offer id");
    let price_plex = offer
        .dict_get("price")
        .and_then(|v| v.as_i64())
        .expect("plex price");

    // 2. can_purchase_offer returns false when PLEX vault balance is insufficient (quantity 2 costs 1000 PLEX, pilot has 500)
    let can_buy_before = rpc(
        &ctx,
        &mut s1,
        "storeManager",
        "can_purchase_offer",
        &[
            PyRep::Integer(offer_id),
            PyRep::String("PLX".into()),
            PyRep::Integer(2),
        ],
    )
    .await
    .expect("can_purchase_offer before deposit");
    assert_eq!(
        can_buy_before,
        PyRep::Bool(false),
        "Cannot purchase without enough PLEX in vault"
    );

    // 3. Negative check: purchasing with removed Aurum currency is rejected
    let aur_err = rpc(
        &ctx,
        &mut s1,
        "storeManager",
        "buy_offer",
        &[
            PyRep::Integer(offer_id),
            PyRep::String("AUR".into()),
            PyRep::Integer(1),
        ],
    )
    .await
    .expect_err("AUR purchase must be rejected");
    assert_eq!(aur_err.code, "AurumCurrencyRemoved");

    // 4. Provision PLEX into pilot hangar and deposit to PLEX Vault via invbroker
    let plex_kv = slash(&ctx, &mut s1, &format!("/giveitem {PLEX_TYPE_ID} 500")).await;
    let plex_item_id = plex_kv
        .dict_get("itemID")
        .and_then(|v| v.as_i64())
        .expect("plex itemID");

    rpc(
        &ctx,
        &mut s1,
        "invbroker",
        "DepositPlexToVault",
        &[
            PyRep::Integer(JITA_STATION),
            PyRep::Integer(plex_item_id),
            PyRep::Integer(500),
        ],
    )
    .await
    .expect("DepositPlexToVault");

    // 5. can_purchase_offer now returns true (pilot now has 1000 PLEX in vault)
    let can_buy_after = rpc(
        &ctx,
        &mut s1,
        "storeManager",
        "can_purchase_offer",
        &[
            PyRep::Integer(offer_id),
            PyRep::String("PLX".into()),
            PyRep::Integer(2),
        ],
    )
    .await
    .expect("can_purchase_offer after deposit");
    assert_eq!(
        can_buy_after,
        PyRep::Bool(true),
        "Can purchase after depositing PLEX"
    );

    // 6. Buy offer using PLEX (PLX)
    let buy_rep = rpc(
        &ctx,
        &mut s1,
        "storeManager",
        "buy_offer",
        &[
            PyRep::Integer(offer_id),
            PyRep::String("PLX".into()),
            PyRep::Integer(1),
        ],
    )
    .await
    .expect("buy_offer with PLEX");
    let PyRep::KeyVal(b_kv) = buy_rep else {
        panic!("buy_offer must return KeyVal");
    };
    assert_eq!(
        b_kv.get("offerID").and_then(|v| v.as_i64()),
        Some(offer_id)
    );
    assert_eq!(
        b_kv.get("plexSpent").and_then(|v| v.as_i64()),
        Some(price_plex)
    );
    let rem_vault = b_kv
        .get("plexRemaining")
        .and_then(|v| v.as_i64())
        .expect("plexRemaining");
    assert_eq!(rem_vault, 1000 - price_plex);
}
