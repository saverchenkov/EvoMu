use super::*;
use crate::router::ServerContext;
use eve_db::{InventoryFlag, InventoryItemRecord, IskCents, WorldRepository};
use eve_proto::PyRep;
use eve_sde::SdeRegistry;
use std::collections::HashMap;
use std::sync::Arc;

fn make_test_context() -> ServerContext {
    let sde = Arc::new(SdeRegistry::with_canonical_seed());
    let db = Arc::new(WorldRepository::with_demo_seed());
    ServerContext::with_sde_and_repository(sde, db)
}

fn char_session(char_id: i64, corp_id: i64) -> SessionState {
    SessionState {
        user_id: Some(1),
        character_id: Some(char_id),
        corporation_id: Some(corp_id),
        solar_system_id: Some(30_000_142),
        station_id: Some(60_003_760),
        ..Default::default()
    }
}

#[test]
fn store_manager_catalog_and_offer_purchase_lifecycle() {
    let ctx = make_test_context();
    let char_id = 90_000_001;
    let station_id = 60_003_760;
    let mut session = char_session(char_id, 98_000_001);
    session.address = "127.0.0.1:26000".into();
    session.station_id = Some(station_id);

    let cats = handle(
        &ctx,
        &mut session,
        "storeManager",
        "get_categories",
        &[],
        &HashMap::new(),
    )
    .unwrap()
    .unwrap();
    assert!(matches!(cats, PyRep::List(ref v) if v.len() == 6));

    let prods = handle(
        &ctx,
        &mut session,
        "storeManager",
        "get_products",
        &[],
        &HashMap::new(),
    )
    .unwrap()
    .unwrap();
    assert!(matches!(prods, PyRep::List(ref v) if v.len() == 3));

    let offers = handle(
        &ctx,
        &mut session,
        "storeManager",
        "get_offers",
        &[],
        &HashMap::new(),
    )
    .unwrap()
    .unwrap();
    assert!(matches!(offers, PyRep::List(ref v) if v.len() == 3));

    let bad_arg_err = handle(
        &ctx,
        &mut session,
        "storeManager",
        "buy_offer",
        &[PyRep::Integer(0)],
        &HashMap::new(),
    )
    .unwrap()
    .unwrap_err();
    assert!(bad_arg_err.code.starts_with("InvalidArg") && bad_arg_err.code.ends_with("ments"));
    assert_eq!(
        handle(
            &ctx,
            &mut session,
            "storeManager",
            "buy_offer",
            &[PyRep::Integer(1002), PyRep::String("AUR".into())],
            &HashMap::new(),
        )
        .unwrap()
        .unwrap_err()
        .code,
        "AurumCurrencyRemoved"
    );
    assert_eq!(
        handle(
            &ctx,
            &mut session,
            "storeManager",
            "buy_offer",
            &[PyRep::Integer(1002), PyRep::String("ISK".into())],
            &HashMap::new(),
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidCurrency"
    );
    assert_eq!(
        handle(
            &ctx,
            &mut session,
            "storeManager",
            "buy_offer",
            &[
                PyRep::Integer(1002),
                PyRep::String("PLX".into()),
                PyRep::Integer(0)
            ],
            &HashMap::new(),
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidQuantity"
    );
    assert_eq!(
        handle(
            &ctx,
            &mut session,
            "storeManager",
            "buy_offer",
            &[PyRep::Integer(999999), PyRep::String("PLX".into())],
            &HashMap::new(),
        )
        .unwrap()
        .unwrap_err()
        .code,
        "OfferNotFound"
    );

    ctx.db.with_mut_snapshot(|snap| {
        let cur = snap.plex_vault_balance(char_id);
        snap.adjust_plex_vault(char_id, -cur);
    });
    let can_buy_before = handle(
        &ctx,
        &mut session,
        "storeManager",
        "can_purchase_offer",
        &[
            PyRep::String("VEVC-1002".into()),
            PyRep::String("PLX".into()),
            PyRep::Integer(1),
        ],
        &HashMap::new(),
    )
    .unwrap()
    .unwrap();
    assert_eq!(can_buy_before.as_bool(), Some(false));

    assert_eq!(
        handle(
            &ctx,
            &mut session,
            "storeManager",
            "buy_offer",
            &[
                PyRep::Integer(1002),
                PyRep::String("PLX".into()),
                PyRep::Integer(1)
            ],
            &HashMap::new(),
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InsufficientPlexBalance"
    );

    ctx.db.with_mut_snapshot(|snap| {
        snap.adjust_plex_vault(char_id, 500);
    });

    let can_buy_after = handle(
        &ctx,
        &mut session,
        "storeManager",
        "can_purchase_offer",
        &[
            PyRep::Integer(1002),
            PyRep::String("PLX".into()),
            PyRep::Integer(2),
        ],
        &HashMap::new(),
    )
    .unwrap()
    .unwrap();
    assert_eq!(can_buy_after.as_bool(), Some(true));

    let bought = handle(
        &ctx,
        &mut session,
        "storeManager",
        "buy_offer",
        &[
            PyRep::Integer(1002),
            PyRep::String("PLX".into()),
            PyRep::Integer(2),
        ],
        &HashMap::new(),
    )
    .unwrap()
    .unwrap();
    assert_eq!(bought.dict_get("success").unwrap().as_bool(), Some(true));
    assert_eq!(bought.dict_get("plexSpent").unwrap().as_i64(), Some(220));
    assert_eq!(
        bought.dict_get("plexRemaining").unwrap().as_i64(),
        Some(280)
    );
    let item_id = bought
        .dict_get("deliveredItemID")
        .unwrap()
        .as_i64()
        .unwrap();
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items[&item_id].type_id, 34599);
        assert_eq!(snap.items[&item_id].quantity, 2);
    });
}

#[test]
fn station_direct_trade_lifecycle_and_validation() {
    let ctx = make_test_context();
    let char_a = 90_000_001;
    let char_b = 90_000_002;
    let station_id = 60_003_760;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ca) = snap.characters.get_mut(&char_a) {
            ca.station_id = Some(station_id);
            ca.balance = IskCents::from_f64_isk(50_000.0);
        }
        let mut cb = snap.characters[&char_a].clone();
        cb.character_id = char_b;
        cb.user_id = 502;
        cb.name = "Partner B".into();
        cb.corporation_id = 98_000_001;
        cb.station_id = Some(station_id);
        cb.balance = IskCents::from_f64_isk(20_000.0);
        snap.characters.insert(char_b, cb);

        snap.items.insert(
            80_001,
            InventoryItemRecord {
                item_id: 80_001,
                type_id: 34,
                owner_id: char_a,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 100,
                singleton: false,
                ..Default::default()
            },
        );
        snap.items.insert(
            80_002,
            InventoryItemRecord {
                item_id: 80_002,
                type_id: 587,
                owner_id: char_b,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });

    let mut sess_a = char_session(char_a, 98_000_001);
    sess_a.station_id = None;
    assert_eq!(
        handle(
            &ctx,
            &mut sess_a,
            "trademgr",
            "InitiateTrade",
            &[PyRep::Integer(char_b)],
            &HashMap::new(),
        )
        .unwrap()
        .unwrap_err()
        .code,
        "NotDockedAtStation"
    );

    sess_a.station_id = Some(station_id);
    assert_eq!(
        handle(
            &ctx,
            &mut sess_a,
            "trademgr",
            "InitiateTrade",
            &[PyRep::Integer(char_a)],
            &HashMap::new(),
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidArgument"
    );

    let init = handle(
        &ctx,
        &mut sess_a,
        "trademgr",
        "InitiateTrade",
        &[PyRep::Integer(char_b)],
        &HashMap::new(),
    )
    .unwrap()
    .unwrap();
    let trade_id = init.dict_get("tradeContainerID").unwrap().as_i64().unwrap();

    let mut sess_b = char_session(char_b, 98_000_001);
    sess_b.station_id = Some(station_id);

    handle(
        &ctx,
        &mut sess_a,
        "trademgr",
        "AddItem",
        &[
            PyRep::Integer(trade_id),
            PyRep::Integer(80_001),
            PyRep::Integer(40),
        ],
        &HashMap::new(),
    )
    .unwrap()
    .unwrap();

    handle(
        &ctx,
        &mut sess_b,
        "trademgr",
        "AddItem",
        &[
            PyRep::Integer(trade_id),
            PyRep::Integer(80_002),
            PyRep::Integer(1),
        ],
        &HashMap::new(),
    )
    .unwrap()
    .unwrap();

    handle(
        &ctx,
        &mut sess_a,
        "trademgr",
        "OfferMoney",
        &[PyRep::Integer(trade_id), PyRep::Real(10_000.0)],
        &HashMap::new(),
    )
    .unwrap()
    .unwrap();

    let offer_a = handle(
        &ctx,
        &mut sess_a,
        "trademgr",
        "MakeOffer",
        &[PyRep::Integer(trade_id), PyRep::Bool(true)],
        &HashMap::new(),
    )
    .unwrap()
    .unwrap();
    assert_eq!(offer_a.dict_get("completed").unwrap().as_bool(), Some(false));

    let offer_b = handle(
        &ctx,
        &mut sess_b,
        "trademgr",
        "MakeOffer",
        &[PyRep::Integer(trade_id), PyRep::Bool(true)],
        &HashMap::new(),
    )
    .unwrap()
    .unwrap();
    assert_eq!(offer_b.dict_get("completed").unwrap().as_bool(), Some(true));

    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items[&80_001].quantity, 60);
        assert_eq!(snap.items[&80_001].owner_id, char_a);
        assert_eq!(snap.items[&80_002].owner_id, char_a);
        assert!(snap
            .items
            .values()
            .any(|i| i.owner_id == char_b && i.type_id == 34 && i.quantity == 40));
        assert_eq!(
            snap.characters[&char_a].balance,
            IskCents::from_f64_isk(40_000.0)
        );
        assert_eq!(
            snap.characters[&char_b].balance,
            IskCents::from_f64_isk(30_000.0)
        );
    });

    assert_eq!(
        handle(
            &ctx,
            &mut sess_a,
            "trademgr",
            "List",
            &[PyRep::Integer(trade_id)],
            &HashMap::new(),
        )
        .unwrap()
        .unwrap_err()
        .code,
        "TradeClosed"
    );
}
