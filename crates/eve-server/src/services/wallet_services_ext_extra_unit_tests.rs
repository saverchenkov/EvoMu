//! Deep branch-coverage and mutation-killing unit tests for `services::wallet_services_ext`.

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::quasar_payment_store::{
    save_store_catalog, save_store_offer_tags, StoreCatalog, StoreCategory, StoreOffer,
    StoreProduct,
};
use crate::services::wallet_services_ext::*;
use eve_db::{
    InventoryFlag, InventoryItemRecord, WorldRepository, WorldStateSnapshot,
};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_sde::SdeRegistry;
use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, OnceLock};

pub(super) static SHARED_SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();

pub(super) fn test_ctx() -> ServerContext {
    if let Some(sde) = SHARED_SDE.get() {
        ServerContext::with_sde_and_repository(
            Arc::clone(sde),
            Arc::new(WorldRepository::with_demo_seed()),
        )
        .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED)
    } else {
        let ctx = ServerContext::new();
        let _ = SHARED_SDE.set(Arc::clone(&ctx.sde));
        ctx
    }
}

pub(super) fn call_wallet_ext(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    handle(ctx, session, service, method, args, kwargs)
}

pub(super) fn put_hangar_item(
    snap: &mut WorldStateSnapshot,
    item_id: i64,
    type_id: u32,
    owner_id: i64,
    station_id: i64,
    quantity: i32,
    singleton: bool,
) {
    snap.items.insert(
        item_id,
        InventoryItemRecord {
            item_id,
            type_id,
            owner_id,
            location_id: station_id,
            flag: InventoryFlag::Hangar,
            quantity,
            singleton,
            version: 1,
            ..Default::default()
        },
    );
}

#[test]
fn note_helpers_and_store_manager_targeted_omega_kwargs_and_overflow_branches() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let st_id = 60_003_760_i64;
    let kw = HashMap::new();

    // 1. Note ID helpers & save_json_note
    assert_eq!(bill_note_id(42), -7_100_000_000_042);
    assert_eq!(autopay_note_id(98_000_001), -7_200_098_000_001);
    ctx.db.with_mut_snapshot(|snap| {
        let bill = CorpBillRecord {
            bill_id: 42,
            bill_type_id: 2,
            debtor_id: 98_000_001,
            creditor_id: 1_000_125,
            amount_cents: 100_000_00,
            due_unix: 2_000_000,
            interest: 0.05,
            external_id: -1,
            external_id2: -1,
            paid: false,
            paid_by_division: None,
        };
        let mut ap = CorpAutoPaySettings {
            division_id: 1,
            enabled_bill_types: BTreeMap::new(),
        };
        ap.enabled_bill_types.insert(2, true);
        save_json_note(snap, bill_note_id(42), -71, 42, "bill:42".into(), &bill, 1000);
        save_json_note(
            snap,
            autopay_note_id(98_000_001),
            -72,
            98_000_001,
            "autopay".into(),
            &ap,
            1000,
        );
        assert!(snap.owner_notes.contains_key(&bill_note_id(42)));
        assert!(snap.owner_notes.contains_key(&autopay_note_id(98_000_001)));
    });

    // 2. Empty session.address does not seed store catalog
    ctx.db.with_mut_snapshot(|snap| {
        snap.quasar_commerce.nes_store_catalog = None;
    });
    let mut sess = SessionState {
        user_id: Some(1),
        character_id: Some(char_id),
        corporation_id: Some(98_000_001),
        station_id: Some(st_id),
        address: String::new(),
        ..Default::default()
    };
    let empty_cats = call_wallet_ext(&ctx, &mut sess, "storeManager", "get_categories", &[], &kw)
        .unwrap()
        .unwrap();
    assert_eq!(empty_cats, PyRep::List(vec![]));

    // 3. Seed custom catalog with:
    // - Offer 2001: targeted offer (excluded from get_offers, rejected when user_id == 0)
    // - Offer 2002: missing product ID 999 (exercises fallback Product 999 in get_offers)
    // - Offer 2003: category 1 Omega license product (type_id == 0, quantity == 1, price_plex == 50)
    // - Offer 2004: free weekly SP offer (price_plex == 0, tag "omega-weekly-free-sp", product type_id == 0, quantity == 25_000)
    ctx.db.with_mut_snapshot(|snap| {
        let cat = StoreCatalog {
            categories: vec![StoreCategory {
                id: 1,
                name: "Game Time".into(),
                description: "Omega".into(),
            }],
            products: vec![
                StoreProduct {
                    id: 201,
                    name: "Omega 30d".into(),
                    description: "Omega".into(),
                    type_id: 0,
                    quantity: 1,
                },
                StoreProduct {
                    id: 202,
                    name: "Weekly SP".into(),
                    description: "SP".into(),
                    type_id: 0,
                    quantity: 25_000,
                },
                StoreProduct {
                    id: 203,
                    name: "Huge Stack".into(),
                    description: "Stack".into(),
                    type_id: 34,
                    quantity: 1_000_000_000,
                },
            ],
            offers: vec![
                StoreOffer {
                    id: 2001,
                    name: "Targeted".into(),
                    description: "Targeted".into(),
                    category_id: 1,
                    price_plex: 10,
                    product_ids: vec![201],
                    targeted: true,
                    price_cents: None,
                    currency: None,
                },
                StoreOffer {
                    id: 2002,
                    name: "FallbackProd".into(),
                    description: "Fallback".into(),
                    category_id: 1,
                    price_plex: 20,
                    product_ids: vec![999],
                    targeted: false,
                    price_cents: None,
                    currency: None,
                },
                StoreOffer {
                    id: 2003,
                    name: "OmegaUpgrade".into(),
                    description: "Omega".into(),
                    category_id: 1,
                    price_plex: 50,
                    product_ids: vec![201],
                    targeted: false,
                    price_cents: None,
                    currency: None,
                },
                StoreOffer {
                    id: 2004,
                    name: "OmegaWeeklySp".into(),
                    description: "Weekly".into(),
                    category_id: 1,
                    price_plex: 0,
                    product_ids: vec![202],
                    targeted: false,
                    price_cents: None,
                    currency: None,
                },
                StoreOffer {
                    id: 2005,
                    name: "OverflowOffer".into(),
                    description: "Overflow".into(),
                    category_id: 1,
                    price_plex: 1,
                    product_ids: vec![203],
                    targeted: false,
                    price_cents: None,
                    currency: None,
                },
            ],
        };
        save_store_catalog(snap, &cat);
        let mut tags = HashMap::new();
        tags.insert(2004, vec!["omega-weekly-free-sp".to_string()]);
        save_store_offer_tags(snap, &tags);
        snap.adjust_plex_vault(char_id, 1_000);
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.clone_grade = 1;
        }
    });

    // get_offers excludes targeted offer 2001 and includes fallback product 999
    let offers_rep = call_wallet_ext(&ctx, &mut sess, "storeManager", "get_offers", &[], &kw)
        .unwrap()
        .unwrap();
    let PyRep::List(offers_list) = offers_rep else {
        panic!("expected list");
    };
    assert_eq!(offers_list.len(), 4);

    // Targeted offer with user_id == 0 -> TargetedOfferNotEligible
    let store_svc = format!("store{}", "Manager");
    sess.user_id = Some(0);
    let mut kw_buy = HashMap::new();
    kw_buy.insert("offerID".to_string(), PyRep::Integer(2001));
    kw_buy.insert("currency".to_string(), PyRep::String("PLEX".into()));
    kw_buy.insert("quantity".to_string(), PyRep::Integer(1));
    let targ_err = call_wallet_ext(&ctx, &mut sess, &store_svc, "buy_offer", &[], &kw_buy)
        .unwrap()
        .unwrap_err();
    assert_eq!(targ_err.code, "TargetedOfferNotEligible");
    sess.user_id = Some(1);

    // AURUM currency rejected via kwargs
    let mut kw_aur = HashMap::new();
    kw_aur.insert("offer_id".to_string(), PyRep::Integer(2003));
    kw_aur.insert("currency".to_string(), PyRep::String("AURUM".into()));
    let aur_err = call_wallet_ext(&ctx, &mut sess, &store_svc, "buy_offer", &[], &kw_aur)
        .unwrap()
        .unwrap_err();
    assert_eq!(aur_err.code, "AurumCurrencyRemoved");

    // Buy category 1 Omega offer 2003 upgrades clone_grade to 2
    let bought_omega = call_wallet_ext(
        &ctx,
        &mut sess,
        "storeManager",
        "buy_offer",
        &[PyRep::Integer(2003), PyRep::String("PLEX".into()), PyRep::Integer(1)],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(bought_omega.dict_get("success"), Some(&PyRep::Bool(true)));
    assert_eq!(ctx.db.with_snapshot(|s| s.characters[&char_id].clone_grade), 2);

    // Buy Omega weekly free SP offer 2004
    assert!(call_wallet_ext(
        &ctx,
        &mut sess,
        "storeManager",
        "buy_offer",
        &[PyRep::Integer(2004)],
        &kw,
    )
    .unwrap()
    .is_ok());

    // Quantity overflow on offer 2005 (1_000_000_000 * 5 > i32::MAX) -> InvalidQuantity
    assert_eq!(
        call_wallet_ext(
            &ctx,
            &mut sess,
            "storeManager",
            "buy_offer",
            &[PyRep::Integer(2005), PyRep::String("PLX".into()), PyRep::Integer(5)],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidQuantity"
    );
}

#[test]
fn store_handlers_inventory_fields_and_direct_trade_partial_and_full_item_versions() {
    let ctx = test_ctx();
    let station_id = 60_003_760_i64;
    let char_a = 90_710_001_i64;
    let char_b = 90_710_002_i64;
    let item_a_partial = 971_101_i64;
    let item_a_full = 971_102_i64;
    let item_b_partial = 971_201_i64;
    let item_b_full = 971_202_i64;

    ctx.db.with_mut_snapshot(|snap| {
        let base_ch = snap.characters.values().next().unwrap().clone();
        for (cid, active_ship) in [(char_a, 971_901_i64), (char_b, 971_902_i64)] {
            let mut ch = base_ch.clone();
            ch.character_id = cid;
            ch.station_id = Some(station_id);
            ch.structure_id = None;
            ch.solar_system_id = 30_000_142;
            ch.active_ship_id = active_ship;
            ch.balance = eve_db::IskCents(100_000_000);
            snap.characters.insert(cid, ch);
        }
        snap.adjust_plex_vault(char_a, 500);
        let cat = StoreCatalog {
            categories: vec![StoreCategory {
                id: 7,
                name: "Ship Skins".into(),
                description: "Cosmetics".into(),
            }],
            products: vec![StoreProduct {
                id: 701,
                name: "Tritanium Crate".into(),
                description: "Crate".into(),
                type_id: 34,
                quantity: 5,
            }],
            offers: vec![StoreOffer {
                id: 7001,
                name: "Tritanium Bundle".into(),
                description: "Bundle".into(),
                category_id: 7,
                price_plex: 15,
                product_ids: vec![701],
                targeted: false,
                price_cents: None,
                currency: None,
            }],
        };
        save_store_catalog(snap, &cat);
        for (iid, owner, qty) in [
            (item_a_partial, char_a, 10_i32),
            (item_a_full, char_a, 4_i32),
            (item_b_partial, char_b, 12_i32),
            (item_b_full, char_b, 6_i32),
        ] {
            snap.items.insert(
                iid,
                InventoryItemRecord {
                    item_id: iid,
                    type_id: 34,
                    owner_id: owner,
                    location_id: station_id,
                    flag: InventoryFlag::Hangar,
                    quantity: qty,
                    singleton: false,
                    version: 3,
                    ..Default::default()
                },
            );
        }
    });

    let mut sess_a = SessionState::new(7101);
    sess_a.user_id = Some(1);
    sess_a.character_id = Some(char_a);
    sess_a.station_id = Some(station_id);
    sess_a.ship_id = Some(971_901);

    let mut sess_b = SessionState::new(7102);
    sess_b.user_id = Some(2);
    sess_b.character_id = Some(char_b);
    sess_b.station_id = Some(station_id);
    sess_b.ship_id = Some(971_902);

    let kw = HashMap::new();
    // 1. Store get_categories, get_products, get_offers (kills L102, L133, L161 FnValue mutants)
    let PyRep::List(cats) = call_wallet_ext(&ctx, &mut sess_a, "storeManager", "get_categories", &[], &kw)
        .unwrap()
        .unwrap()
    else {
        panic!("categories list");
    };
    assert_eq!(cats.len(), 2);
    assert_eq!(cats[0].dict_get("name"), Some(&PyRep::String("Ship Skins".into())));

    let PyRep::List(prods) = call_wallet_ext(&ctx, &mut sess_a, "storeManager", "get_products", &[], &kw)
        .unwrap()
        .unwrap()
    else {
        panic!("products list");
    };
    assert_eq!(prods.len(), 1);
    assert_eq!(prods[0].dict_get("typeID"), Some(&PyRep::Integer(34)));

    let PyRep::List(offers) = call_wallet_ext(&ctx, &mut sess_a, "storeManager", "get_offers", &[], &kw)
        .unwrap()
        .unwrap()
    else {
        panic!("offers list");
    };
    assert_eq!(offers.len(), 1);
    assert_eq!(offers[0].dict_get("offerID"), Some(&PyRep::Integer(7001)));

    // 2. Store can_purchase_offer & buy_offer item delivery field checks (kills L264, L420..427)
    let can = call_wallet_ext(
        &ctx,
        &mut sess_a,
        "storeManager",
        "can_purchase_offer",
        &[PyRep::Integer(7001), PyRep::String("PLEX".into()), PyRep::Integer(3)],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(can, PyRep::Bool(true));

    let buy = call_wallet_ext(
        &ctx,
        &mut sess_a,
        "storeManager",
        "buy_offer",
        &[PyRep::Integer(7001), PyRep::String("PLEX".into()), PyRep::Integer(3)],
        &kw,
    )
    .unwrap()
    .unwrap();
    let delivered_id = buy.dict_get("deliveredItemID").and_then(|v| v.as_i64()).unwrap();
    assert!(delivered_id > 0);
    ctx.db.with_snapshot(|snap| {
        let rec = &snap.items[&delivered_id];
        assert_eq!(rec.item_id, delivered_id);
        assert_eq!(rec.type_id, 34);
        assert_eq!(rec.owner_id, char_a);
        assert_eq!(rec.location_id, station_id);
        assert_eq!(rec.flag, InventoryFlag::Hangar);
        assert_eq!(rec.quantity, 15);
        assert!(!rec.singleton);
        assert_eq!(rec.version, 1);
    });

    // 3. Two-party direct trade with partial and full stack items from both A and B (kills L624, L771, L862, L906)
    let trade = call_wallet_ext(
        &ctx,
        &mut sess_a,
        "trademgr",
        "InitiateTrade",
        &[PyRep::Integer(char_b)],
        &kw,
    )
    .unwrap()
    .unwrap();
    let trade_id = trade.dict_get("tradeContainerID").and_then(|v| v.as_i64()).unwrap();
    assert!(trade_id > 0);

    for (iid, qty) in [(item_a_partial, 3_i64), (item_a_full, 4_i64)] {
        let added = call_wallet_ext(
            &ctx,
            &mut sess_a,
            "trademgr",
            "AddItem",
            &[PyRep::Integer(trade_id), PyRep::Integer(iid), PyRep::Integer(qty)],
            &kw,
        )
        .unwrap()
        .unwrap();
        assert_eq!(added.dict_get("tradeContainerID"), Some(&PyRep::Integer(trade_id)));
    }
    for (iid, qty) in [(item_b_partial, 5_i64), (item_b_full, 6_i64)] {
        let added = call_wallet_ext(
            &ctx,
            &mut sess_b,
            "trademgr",
            "AddItem",
            &[PyRep::Integer(trade_id), PyRep::Integer(iid), PyRep::Integer(qty)],
            &kw,
        )
        .unwrap()
        .unwrap();
        assert_eq!(added.dict_get("tradeContainerID"), Some(&PyRep::Integer(trade_id)));
    }

    let first_accept = call_wallet_ext(
        &ctx,
        &mut sess_a,
        "trademgr",
        "MakeOffer",
        &[PyRep::Integer(trade_id), PyRep::Bool(true)],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(first_accept.dict_get("completed"), Some(&PyRep::Bool(false)));

    let second_accept = call_wallet_ext(
        &ctx,
        &mut sess_b,
        "trademgr",
        "MakeOffer",
        &[PyRep::Integer(trade_id), PyRep::Bool(true)],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(second_accept.dict_get("completed"), Some(&PyRep::Bool(true)));

    ctx.db.with_snapshot(|snap| {
        // Both partial stacks reduced cleanly and bumped version 3 -> 4
        assert_eq!(snap.items[&item_a_partial].quantity, 7);
        assert_eq!(snap.items[&item_a_partial].version, 4);
        assert_eq!(snap.items[&item_b_partial].quantity, 7);
        assert_eq!(snap.items[&item_b_partial].version, 4);

        // Both full stacks changed owner in-place and bumped version 3 -> 4 (kills += -> -= / *=)
        assert_eq!(snap.items[&item_a_full].owner_id, char_b);
        assert_eq!(snap.items[&item_a_full].version, 4);
        assert_eq!(snap.items[&item_b_full].owner_id, char_a);
        assert_eq!(snap.items[&item_b_full].version, 4);
    });
}
