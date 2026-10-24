//! Integration tests for Crate Service (`crateService`), Contextual Offers (`contextualOfferMgr`),
//! and Fast Checkout Service (`FastCheckoutService`).

use crate::common;
use eve_db::{InventoryFlag, InventoryItemRecord};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use std::collections::HashMap;

const CHAR_ID: i64 = 90_000_001;
const STATION_ID: i64 = 60_003_760;

fn get_dict_entry<'a>(rep: &'a PyRep, key: &str) -> Option<&'a PyRep> {
    match rep {
        PyRep::KeyVal(dict) => dict.get(key),
        PyRep::Dict(entries) => entries.iter().find_map(|(k, v)| {
            if let PyRep::String(s) | PyRep::Token(s) = k {
                if s == key {
                    Some(v)
                } else {
                    None
                }
            } else {
                None
            }
        }),
        _ => None,
    }
}

async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let kw = HashMap::new();
    ctx.dispatch_rpc(sess, service, None, method, args, &kw, None).await
}

async fn setup_pilot_session(ctx: &ServerContext) -> SessionState {
    let mut sess = SessionState::new(3402);
    common::login_pilot(ctx, &mut sess, "demo", "demo-secret").await;
    rpc(
        ctx,
        &mut sess,
        "charUnboundMgr",
        "SelectCharacterID",
        &[PyRep::Integer(CHAR_ID)],
    )
    .await
    .expect("SelectCharacterID");
    sess
}

#[tokio::test]
async fn test_crate_service_lifecycle_and_loot_claims() {
    // parity: rpc:crateService.ClaimLoot
    // parity: rpc:crateService.ClaimLootFromCrateStack
    // parity: rpc:crateService.ClaimSkillBundle
    // parity: rpc:crateService.GetCrateLocation
    // parity: rpc:crateService.OpenCrate
    // parity: rpc:crateService.PeekInsideFixedCrate
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let mut sess = setup_pilot_session(&ctx).await;

    // Seed test crate items in inventory
    ctx.db.try_with_mut_snapshot(|snap| {
        // Crate 8001: General loot crate
        snap.items.insert(8001, InventoryItemRecord {
            item_id: 8001,
            type_id: 45000,
            owner_id: CHAR_ID,
            location_id: STATION_ID,
            flag: InventoryFlag::Hangar,
            quantity: 1,
            singleton: false,
            ..Default::default()
        });
        // Crate 8002: Skill bundle crate
        snap.items.insert(8002, InventoryItemRecord {
            item_id: 8002,
            type_id: 45001,
            owner_id: CHAR_ID,
            location_id: STATION_ID,
            flag: InventoryFlag::Hangar,
            quantity: 1,
            singleton: false,
            ..Default::default()
        });
        // Crate 8003: Fitting crate
        snap.items.insert(8003, InventoryItemRecord {
            item_id: 8003,
            type_id: 45002,
            owner_id: CHAR_ID,
            location_id: STATION_ID,
            flag: InventoryFlag::Hangar,
            quantity: 1,
            singleton: false,
            ..Default::default()
        });
        // Crate stack 8004: Stack of 10 crates
        snap.items.insert(8004, InventoryItemRecord {
            item_id: 8004,
            type_id: 45003,
            owner_id: CHAR_ID,
            location_id: STATION_ID,
            flag: InventoryFlag::Hangar,
            quantity: 10,
            singleton: false,
            ..Default::default()
        });
        Ok::<(), ()>(())
    })
    .unwrap();

    // 1. GetCrateLocation
    let loc_rep = rpc(
        &ctx,
        &mut sess,
        "crateService",
        "GetCrateLocation",
        &[PyRep::Integer(8001)],
    )
    .await
    .expect("GetCrateLocation");
    match loc_rep {
        PyRep::Tuple(tuple) => {
            assert_eq!(tuple.len(), 2, "GetCrateLocation returns 2-tuple");
            assert_eq!(tuple[0], PyRep::Integer(STATION_ID));
            assert_eq!(tuple[1], PyRep::Integer(4)); // InventoryFlag::Hangar
        }
        other => panic!("Expected Tuple from GetCrateLocation, got {:?}", other),
    }

    // 2. PeekInsideFixedCrate
    let peek_rep = rpc(
        &ctx,
        &mut sess,
        "crateService",
        "PeekInsideFixedCrate",
        &[PyRep::Integer(8001)],
    )
    .await
    .expect("PeekInsideFixedCrate");
    match peek_rep {
        PyRep::List(list) => {
            assert!(!list.is_empty(), "PeekInsideFixedCrate returns preview loot list");
            if let PyRep::Tuple(loot_tuple) = &list[0] {
                assert_eq!(loot_tuple[0], PyRep::Integer(34)); // Tritanium
                assert_eq!(loot_tuple[1], PyRep::Integer(1000));
            } else {
                panic!("Expected Tuple element in peekResult list");
            }
        }
        other => panic!("Expected List from PeekInsideFixedCrate, got {:?}", other),
    }

    // 3. OpenCrate
    let open_rep = rpc(
        &ctx,
        &mut sess,
        "crateService",
        "OpenCrate",
        &[PyRep::Integer(8001)],
    )
    .await
    .expect("OpenCrate 8001");
    assert_eq!(
        get_dict_entry(&open_rep, "crateID").and_then(|v| v.as_i64()),
        Some(8001)
    );
    assert_eq!(
        get_dict_entry(&open_rep, "opened"),
        Some(&PyRep::Bool(true))
    );

    // 4. ClaimLoot
    let claim_rep = rpc(
        &ctx,
        &mut sess,
        "crateService",
        "ClaimLoot",
        &[PyRep::Integer(8001), PyRep::Integer(STATION_ID), PyRep::Integer(4)],
    )
    .await
    .expect("ClaimLoot 8001");
    assert_eq!(
        get_dict_entry(&claim_rep, "crateID").and_then(|v| v.as_i64()),
        Some(8001)
    );
    let delivered_loot_id = get_dict_entry(&claim_rep, "deliveredItemID")
        .and_then(|v| v.as_i64())
        .expect("deliveredItemID");

    // Second claim must fail
    let repeat_err = rpc(
        &ctx,
        &mut sess,
        "crateService",
        "ClaimLoot",
        &[PyRep::Integer(8001)],
    )
    .await
    .expect_err("Repeat ClaimLoot must fail");
    assert!(matches!(
        repeat_err.code.as_str(),
        "CrateNotOpened" | "LootAlreadyClaimed"
    ));

    // Verify snapshot state: crate item consumed, delivered item exists
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.items.contains_key(&8001), "Opened and claimed crate must be consumed");
        let loot_item = snap.items.get(&delivered_loot_id).expect("Delivered loot item exists");
        assert_eq!(loot_item.owner_id, CHAR_ID);
        assert_eq!(loot_item.type_id, 34);
        assert_eq!(loot_item.quantity, 1000);
    });

    // 5. ClaimSkillBundle
    let _ = rpc(
        &ctx,
        &mut sess,
        "crateService",
        "OpenCrate",
        &[PyRep::Integer(8002)],
    )
    .await
    .expect("OpenCrate 8002");
    let initial_sp = ctx.db.with_snapshot(|snap| {
        snap.characters.get(&CHAR_ID).map(|c| c.free_skill_points).unwrap_or(0.0)
    });
    let bundle_rep = rpc(
        &ctx,
        &mut sess,
        "crateService",
        "ClaimSkillBundle",
        &[PyRep::Integer(8002), PyRep::Integer(1)],
    )
    .await
    .expect("ClaimSkillBundle");
    assert_eq!(
        get_dict_entry(&bundle_rep, "crateID").and_then(|v| v.as_i64()),
        Some(8002)
    );
    assert_eq!(
        get_dict_entry(&bundle_rep, "skillBundleID").and_then(|v| v.as_i64()),
        Some(1)
    );
    ctx.db.with_snapshot(|snap| {
        let ch = snap.characters.get(&CHAR_ID).unwrap();
        assert_eq!(ch.free_skill_points, initial_sp + 25_000.0);
    });

    // 6. ClaimFitting
    let _ = rpc(
        &ctx,
        &mut sess,
        "crateService",
        "OpenCrate",
        &[PyRep::Integer(8003)],
    )
    .await
    .expect("OpenCrate 8003");
    let fit_rep = rpc(
        &ctx,
        &mut sess,
        "crateService",
        "ClaimFitting",
        &[PyRep::Integer(8003), PyRep::Integer(101)],
    )
    .await
    .expect("ClaimFitting");
    assert_eq!(
        get_dict_entry(&fit_rep, "crateID").and_then(|v| v.as_i64()),
        Some(8003)
    );
    assert_eq!(
        get_dict_entry(&fit_rep, "fittingID").and_then(|v| v.as_i64()),
        Some(101)
    );
    assert_eq!(
        get_dict_entry(&fit_rep, "claimed"),
        Some(&PyRep::Bool(true))
    );

    // Repeat fitting claim must fail
    let fit_repeat_err = rpc(
        &ctx,
        &mut sess,
        "crateService",
        "ClaimFitting",
        &[PyRep::Integer(8003), PyRep::Integer(101)],
    )
    .await
    .expect_err("Repeat ClaimFitting must fail");
    assert_eq!(fit_repeat_err.code, "FittingAlreadyClaimed");

    // 7. ClaimLootFromCrateStack
    let stack_rep = rpc(
        &ctx,
        &mut sess,
        "crateService",
        "ClaimLootFromCrateStack",
        &[PyRep::Integer(8004), PyRep::Integer(3)],
    )
    .await
    .expect("ClaimLootFromCrateStack");
    match stack_rep {
        PyRep::Tuple(t) => {
            assert_eq!(t.len(), 2, "ClaimLootFromCrateStack returns 2-tuple");
            assert_eq!(t[0], PyRep::Integer(3), "cratesOpened = 3");
            assert_eq!(t[1], PyRep::Integer(3), "numCratesToOpen = 3");
        }
        other => panic!("Expected Tuple, got {:?}", other),
    }

    // Verify crate stack reduced from 10 to 7
    ctx.db.with_snapshot(|snap| {
        let crate_item = snap.items.get(&8004).expect("Crate stack 8004 remains");
        assert_eq!(crate_item.quantity, 7, "10 - 3 = 7 crates remaining");
    });
}

#[tokio::test]
async fn test_contextual_offers_telemetry_tracking() {
    // parity: rpc:contextualOfferMgr.TrackOfferBuyPress
    // parity: rpc:contextualOfferMgr.TrackOfferClosed
    // parity: rpc:contextualOfferMgr.TrackOfferDelivered
    // parity: rpc:contextualOfferMgr.TrackOfferPurchased
    // parity: rpc:contextualOfferMgr.TrackOfferPurchasedSeen
    // parity: rpc:contextualOfferMgr.TrackOfferSeen
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let mut sess = setup_pilot_session(&ctx).await;
    let offer_id = 701i64;

    // Call each of the 6 telemetry methods
    let r1 = rpc(
        &ctx,
        &mut sess,
        "contextualOfferMgr",
        "TrackOfferDelivered",
        &[PyRep::Integer(offer_id)],
    )
    .await
    .expect("TrackOfferDelivered");
    assert_eq!(r1, PyRep::None);

    let r2 = rpc(
        &ctx,
        &mut sess,
        "contextualOfferMgr",
        "TrackOfferSeen",
        &[PyRep::Integer(offer_id)],
    )
    .await
    .expect("TrackOfferSeen");
    assert_eq!(r2, PyRep::None);

    let r3 = rpc(
        &ctx,
        &mut sess,
        "contextualOfferMgr",
        "TrackOfferBuyPress",
        &[PyRep::Integer(offer_id)],
    )
    .await
    .expect("TrackOfferBuyPress");
    assert_eq!(r3, PyRep::None);

    let r4 = rpc(
        &ctx,
        &mut sess,
        "contextualOfferMgr",
        "TrackOfferPurchased",
        &[PyRep::Integer(offer_id)],
    )
    .await
    .expect("TrackOfferPurchased");
    assert_eq!(r4, PyRep::None);

    let r5 = rpc(
        &ctx,
        &mut sess,
        "contextualOfferMgr",
        "TrackOfferPurchasedSeen",
        &[PyRep::Integer(offer_id)],
    )
    .await
    .expect("TrackOfferPurchasedSeen");
    assert_eq!(r5, PyRep::None);

    let r6 = rpc(
        &ctx,
        &mut sess,
        "contextualOfferMgr",
        "TrackOfferClosed",
        &[PyRep::Integer(offer_id)],
    )
    .await
    .expect("TrackOfferClosed");
    assert_eq!(r6, PyRep::None);

    // Verify all 6 events were recorded in snapshot
    ctx.db.with_snapshot(|snap| {
        use eve_server::services::cosmetics_ui_ops::load_cosmetics_ui_state;
        let ext = load_cosmetics_ui_state(snap);
        let events = ext.contextual_offer_events.get(&CHAR_ID).expect("Events recorded");
        assert_eq!(events.len(), 6, "Expected 6 tracked offer telemetry events");
        let methods: Vec<&str> = events.iter().map(|(m, _, _)| m.as_str()).collect();
        assert_eq!(
            methods,
            vec![
                "TrackOfferDelivered",
                "TrackOfferSeen",
                "TrackOfferBuyPress",
                "TrackOfferPurchased",
                "TrackOfferPurchasedSeen",
                "TrackOfferClosed"
            ]
        );
        for (_, oid, _) in events {
            assert_eq!(*oid, offer_id);
        }
    });
}

    // parity: rpc:FastCheckoutService.BuyOffer
    // parity: rpc:FastCheckoutService.ClearFastCheckoutCacheForUser
    // parity: rpc:FastCheckoutService.GetOffersForUser
    // parity: rpc:FastCheckoutService.GetTestingConfiguration
    // parity: rpc:FastCheckoutService.IsFastCheckoutEnabledForUser
#[tokio::test]
async fn test_fast_checkout_service_configuration_and_disablement() {
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let mut sess = setup_pilot_session(&ctx).await;

    // 1. IsFastCheckoutEnabledForUser (real money checkout disabled on self-hosted server)
    let is_enabled = rpc(
        &ctx,
        &mut sess,
        "FastCheckoutService",
        "IsFastCheckoutEnabledForUser",
        &[],
    )
    .await
    .expect("IsFastCheckoutEnabledForUser");
    assert_eq!(is_enabled, PyRep::Bool(false));

    // 2. GetOffersForUser (empty list when disabled)
    let offers = rpc(
        &ctx,
        &mut sess,
        "FastCheckoutService",
        "GetOffersForUser",
        &[],
    )
    .await
    .expect("GetOffersForUser");
    assert_eq!(offers, PyRep::List(Vec::new()));

    // 3. GetTestingConfiguration (3-element tuple for unpack)
    let cfg = rpc(
        &ctx,
        &mut sess,
        "FastCheckoutService",
        "GetTestingConfiguration",
        &[],
    )
    .await
    .expect("GetTestingConfiguration");
    match cfg {
        PyRep::Tuple(t) => {
            assert_eq!(t.len(), 3, "GetTestingConfiguration returns 3-tuple");
            assert_eq!(t[0], PyRep::Bool(false)); // _is_china_funnel_enabled
            assert_eq!(t[1], PyRep::String("".into())); // _fake_buy_plex_offer_url
            assert_eq!(t[2], PyRep::Bool(false)); // _use_shell_execute_to_buy_plex_offer
        }
        other => panic!("Expected Tuple, got {:?}", other),
    }

    // 4. ClearFastCheckoutCacheForUser
    let clear_rep = rpc(
        &ctx,
        &mut sess,
        "FastCheckoutService",
        "ClearFastCheckoutCacheForUser",
        &[],
    )
    .await
    .expect("ClearFastCheckoutCacheForUser");
    assert_eq!(clear_rep, PyRep::None);

    // 5. BuyOffer (returns FastCheckoutDisabled error)
    let buy_err = rpc(
        &ctx,
        &mut sess,
        "FastCheckoutService",
        "BuyOffer",
        &[
            PyRep::Integer(1001),
            PyRep::Integer(1),
            PyRep::String("password".into()),
            PyRep::String("trace-uuid".into()),
            PyRep::String("journey-uuid".into()),
        ],
    )
    .await
    .expect_err("BuyOffer must return FastCheckoutDisabled");
    assert_eq!(buy_err.code, "FastCheckoutDisabled");
}
