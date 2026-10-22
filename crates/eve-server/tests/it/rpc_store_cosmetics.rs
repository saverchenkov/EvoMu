//! Integration tests for New Eden Store (`storeManager`) and Ship Cosmetics (`shipCosmeticsMgr`).

use crate::common;
use eve_db::service_roles::ROLE_GM_DEFAULT;
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
    let mut sess = SessionState::new(3401);
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
async fn test_store_manager_categories_offers_and_purchases() {
    // parity: rpc:storeManager.buy_offer
    // parity: rpc:storeManager.can_purchase_offer
    // parity: rpc:storeManager.get_categories
    // parity: rpc:storeManager.get_offers
    // parity: rpc:storeManager.get_products
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let mut sess = setup_pilot_session(&ctx).await;

    // Seed PLEX vault with 1,000 PLEX
    ctx.db.try_with_mut_snapshot(|snap| {
        let _ = snap.adjust_plex_vault(CHAR_ID, 1000);
        Ok::<(), ()>(())
    })
    .unwrap();

    // 1. get_categories
    let cat_rep = rpc(&ctx, &mut sess, "storeManager", "get_categories", &[])
        .await
        .expect("get_categories");
    let cats = match cat_rep {
        PyRep::List(list) => list,
        other => panic!("Expected List for get_categories, got {:?}", other),
    };
    assert!(cats.len() >= 3, "Expected at least 3 NES categories");
    let has_omega_cat = cats.iter().any(|c| {
        get_dict_entry(c, "id").and_then(|v| v.as_i64()) == Some(1)
            && get_dict_entry(c, "name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .contains("Omega")
    });
    assert!(has_omega_cat, "Expected Omega Clone State category in get_categories");

    // 2. get_products & get_offers
    let prod_rep = rpc(&ctx, &mut sess, "storeManager", "get_products", &[])
        .await
        .expect("get_products");
    let prods = match prod_rep {
        PyRep::List(list) => list,
        other => panic!("Expected List for get_products, got {:?}", other),
    };
    assert!(!prods.is_empty(), "Expected non-empty products list");

    let offers_rep = rpc(&ctx, &mut sess, "storeManager", "get_offers", &[])
        .await
        .expect("get_offers");
    let offers = match offers_rep {
        PyRep::List(list) => list,
        other => panic!("Expected List for get_offers, got {:?}", other),
    };
    assert_eq!(prods.len(), offers.len(), "get_products and get_offers should return matching catalog");

    let skin_offer = offers
        .iter()
        .find(|o| get_dict_entry(o, "offerID").and_then(|v| v.as_i64()) == Some(1002))
        .expect("Offer 1002 (Ship SKIN) present in offers catalog");
    assert_eq!(
        get_dict_entry(skin_offer, "currency").and_then(|v| v.as_str()),
        Some("PLX")
    );
    assert_eq!(
        get_dict_entry(skin_offer, "price").and_then(|v| v.as_i64()),
        Some(110)
    );

    // 3. can_purchase_offer (affordable vs unaffordable)
    let can_buy_skin = rpc(
        &ctx,
        &mut sess,
        "storeManager",
        "can_purchase_offer",
        &[PyRep::Integer(1002)],
    )
    .await
    .expect("can_purchase_offer 1002");
    assert_eq!(can_buy_skin, PyRep::Bool(true), "Pilot with 1000 PLEX can buy 110 PLEX skin");

    // Also test string offer ID format "VEVC-1002" used by nodegraph conditions
    let can_buy_skin_str = rpc(
        &ctx,
        &mut sess,
        "storeManager",
        "can_purchase_offer",
        &[PyRep::String("VEVC-1002".into())],
    )
    .await
    .expect("can_purchase_offer VEVC-1002");
    assert_eq!(can_buy_skin_str, PyRep::Bool(true));

    // Unaffordable quantity (10x Omega = 5,000 PLEX > 1,000 PLEX in vault)
    let can_buy_bulk = rpc(
        &ctx,
        &mut sess,
        "storeManager",
        "can_purchase_offer",
        &[PyRep::Integer(1001), PyRep::String("PLX".into()), PyRep::Integer(10)],
    )
    .await
    .expect("can_purchase_offer bulk");
    assert_eq!(can_buy_bulk, PyRep::Bool(false), "Cannot afford 5,000 PLEX with 1,000 PLEX balance");

    // 4. buy_offer with legacy Aurum currency rejection
    let aur_err = rpc(
        &ctx,
        &mut sess,
        "storeManager",
        "buy_offer",
        &[PyRep::Integer(1002), PyRep::String("AUR".into()), PyRep::Integer(1)],
    )
    .await
    .expect_err("AUR currency purchase must fail");
    assert_eq!(aur_err.code, "AurumCurrencyRemoved");

    // 5. buy_offer with PLEX (successful execution)
    let vault_before = ctx.db.with_snapshot(|snap| snap.plex_vault_balance(CHAR_ID));
    let buy_res = rpc(
        &ctx,
        &mut sess,
        "storeManager",
        "buy_offer",
        &[PyRep::Integer(1002), PyRep::String("PLX".into()), PyRep::Integer(1)],
    )
    .await
    .expect("buy_offer 1002");
    assert_eq!(get_dict_entry(&buy_res, "success"), Some(&PyRep::Bool(true)));
    assert_eq!(
        get_dict_entry(&buy_res, "offerID").and_then(|v| v.as_i64()),
        Some(1002)
    );
    assert_eq!(
        get_dict_entry(&buy_res, "plexSpent").and_then(|v| v.as_i64()),
        Some(110)
    );
    assert_eq!(
        get_dict_entry(&buy_res, "plexRemaining").and_then(|v| v.as_i64()),
        Some(vault_before - 110)
    );
    let delivered_id = get_dict_entry(&buy_res, "deliveredItemID")
        .and_then(|v| v.as_i64())
        .expect("deliveredItemID");
    assert!(delivered_id > 0);

    // Verify snapshot state
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.plex_vault_balance(CHAR_ID), vault_before - 110);
        let delivered_item = snap.items.get(&delivered_id).expect("delivered item in items");
        assert_eq!(delivered_item.owner_id, CHAR_ID);
        assert_eq!(delivered_item.location_id, STATION_ID);
        assert_eq!(delivered_item.quantity, 1);
    });
}

#[tokio::test]
async fn test_ship_cosmetics_manager_licenses_and_first_party() {
    // parity: rpc:shipCosmeticsMgr.GMExpireSkinLicense
    // parity: rpc:shipCosmeticsMgr.GetFirstPartySkinData
    // parity: rpc:shipCosmeticsMgr.GetLicencedSkins
    // parity: rpc:shipCosmeticsMgr.GetLicencedSkinsForShipType
    // parity: rpc:shipCosmeticsMgr.RemoveSkin
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let mut sess = setup_pilot_session(&ctx).await;

    // SDE skin 5 is for Megathron (typeID 641, materialID 3)
    let skin_id: u32 = 5;
    let megathron_type: u32 = 641;
    let rifter_type: u32 = 587;

    // Seed skin license for character in snapshot
    ctx.db.try_with_mut_snapshot(|snap| {
        snap.licensed_skins.entry(CHAR_ID).or_default().push(skin_id);
        Ok::<(), ()>(())
    })
    .unwrap();

    // 1. GetLicencedSkins
    let skins_rep = rpc(&ctx, &mut sess, "shipCosmeticsMgr", "GetLicencedSkins", &[])
        .await
        .expect("GetLicencedSkins");
    let skins_list = match skins_rep {
        PyRep::List(list) => list,
        other => panic!("Expected List for GetLicencedSkins, got {:?}", other),
    };
    assert!(!skins_list.is_empty(), "Expected licensed skin in list");
    let skin_entry = skins_list
        .iter()
        .find(|s| get_dict_entry(s, "skinID").and_then(|v| v.as_i64()) == Some(skin_id as i64))
        .expect("Skin 5 present in licensed skins");
    assert_eq!(
        get_dict_entry(skin_entry, "skinMaterialID").and_then(|v| v.as_i64()),
        Some(3)
    );
    assert_eq!(
        get_dict_entry(skin_entry, "isPermanent"),
        Some(&PyRep::Bool(true))
    );

    // 2. GetLicencedSkinsForShipType
    let mega_skins = rpc(
        &ctx,
        &mut sess,
        "shipCosmeticsMgr",
        "GetLicencedSkinsForShipType",
        &[PyRep::Integer(megathron_type as i64)],
    )
    .await
    .expect("GetLicencedSkinsForShipType Megathron");
    match mega_skins {
        PyRep::List(list) => assert_eq!(list.len(), 1, "Expected 1 skin for Megathron"),
        other => panic!("Expected List, got {:?}", other),
    }

    let rifter_skins = rpc(
        &ctx,
        &mut sess,
        "shipCosmeticsMgr",
        "GetLicencedSkinsForShipType",
        &[PyRep::Integer(rifter_type as i64)],
    )
    .await
    .expect("GetLicencedSkinsForShipType Rifter");
    match rifter_skins {
        PyRep::List(list) => assert!(list.is_empty(), "Skin 5 does not apply to Rifter"),
        other => panic!("Expected empty List, got {:?}", other),
    }

    // 3. GetFirstPartySkinData
    let first_party = rpc(
        &ctx,
        &mut sess,
        "shipCosmeticsMgr",
        "GetFirstPartySkinData",
        &[PyRep::Integer(CHAR_ID), PyRep::Integer(skin_id as i64)],
    )
    .await
    .expect("GetFirstPartySkinData");
    assert_eq!(
        get_dict_entry(&first_party, "skinID").and_then(|v| v.as_i64()),
        Some(skin_id as i64)
    );
    assert_eq!(
        get_dict_entry(&first_party, "licenseeID").and_then(|v| v.as_i64()),
        Some(CHAR_ID)
    );
    assert_eq!(
        get_dict_entry(&first_party, "isSingleUse"),
        Some(&PyRep::Bool(false))
    );

    // Unlicensed skin query returns None
    let unlic = rpc(
        &ctx,
        &mut sess,
        "shipCosmeticsMgr",
        "GetFirstPartySkinData",
        &[PyRep::Integer(CHAR_ID), PyRep::Integer(99999)],
    )
    .await
    .expect("GetFirstPartySkinData unlicensed");
    assert_eq!(unlic, PyRep::None);

    // 4. Role enforcement on GM actions
    let non_gm_err = rpc(
        &ctx,
        &mut sess,
        "shipCosmeticsMgr",
        "GMExpireSkinLicense",
        &[PyRep::Integer(skin_id as i64)],
    )
    .await
    .expect_err("Non-GM cannot call GMExpireSkinLicense");
    assert_eq!(non_gm_err.code, "InsufficientRole");

    let non_gm_rem_err = rpc(
        &ctx,
        &mut sess,
        "shipCosmeticsMgr",
        "RemoveSkin",
        &[PyRep::Integer(skin_id as i64)],
    )
    .await
    .expect_err("Non-GM cannot call RemoveSkin");
    assert_eq!(non_gm_rem_err.code, "InsufficientRole");

    // 5. GMExpireSkinLicense as GM
    sess.role = ROLE_GM_DEFAULT;
    let expire_res = rpc(
        &ctx,
        &mut sess,
        "shipCosmeticsMgr",
        "GMExpireSkinLicense",
        &[PyRep::Integer(skin_id as i64)],
    )
    .await
    .expect("GMExpireSkinLicense as GM");
    assert_eq!(expire_res, PyRep::Bool(true));

    // Confirm skin removed from snapshot
    ctx.db.with_snapshot(|snap| {
        let skins = snap.licensed_skins.get(&CHAR_ID).cloned().unwrap_or_default();
        assert!(!skins.contains(&skin_id), "Skin 5 should be expired/removed");
    });

    // 6. RemoveSkin as GM
    ctx.db.try_with_mut_snapshot(|snap| {
        snap.licensed_skins.entry(CHAR_ID).or_default().push(skin_id);
        Ok::<(), ()>(())
    })
    .unwrap();

    let rem_res = rpc(
        &ctx,
        &mut sess,
        "shipCosmeticsMgr",
        "RemoveSkin",
        &[PyRep::Integer(skin_id as i64), PyRep::Integer(CHAR_ID)],
    )
    .await
    .expect("RemoveSkin as GM");
    assert_eq!(rem_res, PyRep::Bool(true));

    ctx.db.with_snapshot(|snap| {
        let skins = snap.licensed_skins.get(&CHAR_ID).cloned().unwrap_or_default();
        assert!(!skins.contains(&skin_id), "Skin 5 should be removed by RemoveSkin");
    });
}
