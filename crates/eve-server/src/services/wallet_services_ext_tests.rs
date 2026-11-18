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
    assert!(matches!(cats, PyRep::List(ref v) if v.len() == 8));

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
    assert!(matches!(prods, PyRep::List(ref v) if v.len() >= 5));

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
    assert!(matches!(offers, PyRep::List(ref v) if v.len() >= 4));

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
        if let Some(mut acc) = snap.accounts.values().next().cloned() {
            acc.user_id = 502;
            acc.username = "user_502".into();
            snap.accounts.insert("user_502".into(), acc);
        }
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
    sess_a.session_id = 101;
    sess_a.station_id = None;
    ctx.db
        .acquire_online_lock(502, 102, Some(char_b), false, ctx.now_unix())
        .unwrap();
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
    let trade_bid = init.as_i64().unwrap();
    let trade_id = handle(
        &ctx,
        &mut sess_a,
        "trademgr",
        "List",
        &[PyRep::Integer(trade_bid)],
        &HashMap::new(),
    )
    .unwrap()
    .unwrap()
    .dict_get("tradeContainerID")
    .unwrap()
    .as_i64()
    .unwrap();

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
        "CustomInfo"
    );
}

#[test]
fn station_direct_trade_completes_when_current_trade_item_is_escrowed_and_emits_item_change() {
    let ctx = make_test_context();
    let station_id: i64 = 60_003_760;
    let char_a: i64 = 90_000_001;
    let char_b: i64 = 90_000_002;
    let item_id: i64 = 80_611;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(mut acc_a) = snap.accounts.values().next().cloned() {
            acc_a.user_id = 501;
            acc_a.username = "user_a".into();
            snap.accounts.insert("user_a".into(), acc_a.clone());
            acc_a.user_id = 502;
            acc_a.username = "user_b".into();
            snap.accounts.insert("user_b".into(), acc_a);
        }
        if let Some(ca) = snap.characters.get_mut(&char_a) {
            ca.user_id = 501;
            ca.station_id = Some(station_id);
            ca.balance = IskCents::from_f64_isk(5_000.0);
        }
        let mut cb = snap.characters[&char_a].clone();
        cb.character_id = char_b;
        cb.user_id = 502;
        cb.name = "BuyerB".into();
        cb.corporation_id = 98_000_001;
        cb.station_id = Some(station_id);
        cb.balance = IskCents::from_f64_isk(10_000.0);
        snap.characters.insert(char_b, cb);
        snap.items.insert(
            item_id,
            InventoryItemRecord {
                item_id,
                type_id: 587,
                owner_id: char_a,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });
    let mut sess_a = char_session(char_a, 98_000_001);
    sess_a.user_id = Some(501);
    sess_a.station_id = Some(station_id);
    let mut sess_b = char_session(char_b, 98_000_001);
    sess_b.user_id = Some(502);
    sess_b.station_id = Some(station_id);
    ctx.db
        .acquire_online_lock(501, 101, Some(char_a), false, ctx.now_unix())
        .unwrap();
    ctx.db
        .acquire_online_lock(502, 102, Some(char_b), false, ctx.now_unix())
        .unwrap();

    let init_trade_container_id = |sess: &mut SessionState, target: i64| {
        let init = handle(
            &ctx,
            sess,
            "trademgr",
            "InitiateTrade",
            &[PyRep::Integer(target)],
            &HashMap::new(),
        )
        .unwrap()
        .unwrap();
        let bid = init.as_i64().unwrap();
        handle(&ctx, sess, "trademgr", "List", &[PyRep::Integer(bid)], &HashMap::new())
            .unwrap()
            .unwrap()
            .dict_get("tradeContainerID")
            .unwrap()
            .as_i64()
            .unwrap()
    };

    let trade_id = init_trade_container_id(&mut sess_a, char_b);
    let _ = ctx.drain_notifications_for_char(char_a);
    let _ = ctx.drain_notifications_for_char(char_b);

    handle(&ctx, &mut sess_a, "trademgr", "AddItem", &[PyRep::Integer(trade_id), PyRep::Integer(item_id), PyRep::Integer(1)], &HashMap::new())
        .unwrap()
        .unwrap();
    handle(&ctx, &mut sess_b, "trademgr", "OfferMoney", &[PyRep::Integer(trade_id), PyRep::Real(2_500.0)], &HashMap::new())
        .unwrap()
        .unwrap();

    handle(&ctx, &mut sess_a, "trademgr", "MakeOffer", &[PyRep::Integer(trade_id), PyRep::Bool(true)], &HashMap::new())
        .unwrap()
        .unwrap();
    let res = handle(&ctx, &mut sess_b, "trademgr", "MakeOffer", &[PyRep::Integer(trade_id), PyRep::Bool(true)], &HashMap::new())
        .unwrap()
        .unwrap();
    assert_eq!(res.dict_get("completed").and_then(PyRep::as_bool), Some(true));

    ctx.db.with_snapshot(|s| {
        assert_eq!(s.items[&item_id].owner_id, char_b);
        assert!(!s.is_item_locked_excluding_trade(item_id, None));
        assert!(
            !s.owner_notes.contains_key(&super::trade_note_id(trade_id)),
            "completed trade must remove -73 trade owner note"
        );
    });
    assert!(ctx.drain_notifications_for_char(char_a).iter().any(|n| n.event == "OnItemChange"));
    assert!(ctx.drain_notifications_for_char(char_b).iter().any(|n| n.event == "OnItemChange"));

    // #848: Abort removes -73 trade note so List returns TradeNotFound
    let trade_id2 = init_trade_container_id(&mut sess_a, char_b);
    assert!(handle(&ctx, &mut sess_a, "trademgr", "List", &[PyRep::Integer(trade_id2)], &HashMap::new()).unwrap().is_ok());
    handle(&ctx, &mut sess_a, "trademgr", "Abort", &[PyRep::Integer(trade_id2)], &HashMap::new())
        .unwrap()
        .unwrap();
    assert!(handle(&ctx, &mut sess_a, "trademgr", "List", &[PyRep::Integer(trade_id2)], &HashMap::new()).unwrap().is_err());
    assert!(ctx.db.with_snapshot(|s| !s.owner_notes.contains_key(&super::trade_note_id(trade_id2))));

    // #848: Logout cancels open trade, unlocks items, and emits OnTradeCancel
    let trade_id3 = init_trade_container_id(&mut sess_a, char_b);
    handle(&ctx, &mut sess_b, "trademgr", "AddItem", &[PyRep::Integer(trade_id3), PyRep::Integer(item_id), PyRep::Integer(1)], &HashMap::new())
        .unwrap()
        .unwrap();
    assert!(ctx.db.with_snapshot(|s| s.is_item_locked_excluding_trade(item_id, None)));
    let _ = ctx.drain_notifications_for_char(char_b);
    crate::services::fleet_social::fleet_social_notify::on_character_logout(&ctx, char_a);
    assert!(!ctx.db.with_snapshot(|s| s.is_item_locked_excluding_trade(item_id, None)));
    assert!(ctx.db.with_snapshot(|s| !s.owner_notes.contains_key(&super::trade_note_id(trade_id3))));
    assert!(handle(&ctx, &mut sess_b, "trademgr", "List", &[PyRep::Integer(trade_id3)], &HashMap::new()).unwrap().is_err());
    assert!(ctx.drain_notifications_for_char(char_b).iter().any(|n| n.event == "OnTradeCancel"));

    // #848: Undocking mid-trade still commits -73 note removal & emits OnTradeCancel while returning NotDockedAtStation
    let _ = ctx.drain_notifications_for_char(char_a);
    let _ = ctx.drain_notifications_for_char(char_b);
    let trade_id4 = init_trade_container_id(&mut sess_a, char_b);
    ctx.db.with_mut_snapshot(|s| {
        if let Some(cb) = s.characters.get_mut(&char_b) {
            cb.station_id = None;
        }
    });
    let err = handle(&ctx, &mut sess_a, "trademgr", "OfferMoney", &[PyRep::Integer(trade_id4), PyRep::Real(100.0)], &HashMap::new())
        .unwrap()
        .unwrap_err();
    assert_eq!(err.code, "NotDockedAtStation");
    assert!(ctx.db.with_snapshot(|s| !s.owner_notes.contains_key(&super::trade_note_id(trade_id4))));
    assert!(ctx.drain_notifications_for_char(char_b).iter().any(|n| n.event == "OnTradeCancel"));
}

#[test]
fn station_direct_trade_rejects_offline_duplicate_and_blocked_and_cleans_bounds() {
    let ctx = make_test_context();
    let station_id: i64 = 60_003_760;
    let char_a: i64 = 90_000_001;
    let char_b: i64 = 90_000_002;
    let char_c: i64 = 90_000_003;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(mut acc) = snap.accounts.values().next().cloned() {
            for (uid, uname) in [(501_u64, "t_501"), (502_u64, "t_502"), (503_u64, "t_503")] {
                acc.user_id = uid;
                acc.username = uname.into();
                snap.accounts.insert(uname.into(), acc.clone());
            }
        }
        if let Some(ca) = snap.characters.get_mut(&char_a) {
            ca.user_id = 501;
            ca.station_id = Some(station_id);
        }
        let mut cb = snap.characters[&char_a].clone();
        cb.character_id = char_b;
        cb.user_id = 502;
        cb.name = "TraderB".into();
        cb.station_id = Some(station_id);
        snap.characters.insert(char_b, cb);

        let mut cc = snap.characters[&char_a].clone();
        cc.character_id = char_c;
        cc.user_id = 503;
        cc.name = "TraderC".into();
        cc.station_id = Some(station_id);
        snap.characters.insert(char_c, cc);
    });

    let mut sess_a = SessionState {
        session_id: 901,
        user_id: Some(501),
        character_id: Some(char_a),
        corporation_id: Some(98_000_001),
        solar_system_id: Some(30_000_142),
        station_id: Some(station_id),
        ..Default::default()
    };
    let mut sess_c = SessionState {
        session_id: 903,
        user_id: Some(503),
        character_id: Some(char_c),
        corporation_id: Some(98_000_001),
        solar_system_id: Some(30_000_142),
        station_id: Some(station_id),
        ..Default::default()
    };
    ctx.db
        .acquire_online_lock(501, 901, Some(char_a), false, ctx.now_unix())
        .unwrap();
    ctx.db
        .acquire_online_lock(503, 903, Some(char_c), false, ctx.now_unix())
        .unwrap();

    // 1. Offline partner rejected without creating trade note or bound objects
    let offline_err = handle(
        &ctx,
        &mut sess_a,
        "trademgr",
        "InitiateTrade",
        &[PyRep::Integer(char_b)],
        &HashMap::new(),
    )
    .unwrap()
    .unwrap_err();
    assert_eq!(offline_err.code, "PartnerNotOnline");
    assert!(ctx.bound_objects.read().unwrap().is_empty());
    assert!(ctx
        .db
        .with_snapshot(|s| !s.owner_notes.values().any(|n| n.character_id == -73)));

    // Bring char_b online
    ctx.db
        .acquire_online_lock(502, 902, Some(char_b), false, ctx.now_unix())
        .unwrap();

    // 2. Contact blocklist rejects InitiateTrade when partner blocked initiator
    ctx.esi_state.char_contacts.write().unwrap().insert(
        char_b,
        vec![crate::esi::runtime_state::EsiContactEntry {
            contact_id: char_a,
            standing: -10.0,
            contact_type: "character".into(),
            is_watched: false,
            is_blocked: true,
            label_ids: vec![],
        }],
    );
    let blocked_err = handle(
        &ctx,
        &mut sess_a,
        "trademgr",
        "InitiateTrade",
        &[PyRep::Integer(char_b)],
        &HashMap::new(),
    )
    .unwrap()
    .unwrap_err();
    assert_eq!(blocked_err.code, "ContactBlocked");
    ctx.esi_state.char_contacts.write().unwrap().clear();

    // 3. Successful trade emits OnTradeInitiate ONLY to partner, and blocks duplicates for both parties
    let _ = ctx.drain_notifications_for_char(char_a);
    let _ = ctx.drain_notifications_for_char(char_b);
    let bid_a = handle(
        &ctx,
        &mut sess_a,
        "trademgr",
        "InitiateTrade",
        &[PyRep::Integer(char_b)],
        &HashMap::new(),
    )
    .unwrap()
    .unwrap()
    .as_i64()
    .unwrap();
    assert!(
        !ctx.drain_notifications_for_char(char_a)
            .iter()
            .any(|n| n.event == "OnTradeInitiate"),
        "Initiator must receive tradeSession from CallRsp, not duplicate OnTradeInitiate"
    );
    assert_eq!(
        ctx.drain_notifications_for_char(char_b)
            .iter()
            .filter(|n| n.event == "OnTradeInitiate")
            .count(),
        1
    );

    let dup_self_err = handle(
        &ctx,
        &mut sess_a,
        "trademgr",
        "InitiateTrade",
        &[PyRep::Integer(char_c)],
        &HashMap::new(),
    )
    .unwrap()
    .unwrap_err();
    assert_eq!(dup_self_err.code, "TradeAlreadyActive");

    let dup_partner_err = handle(
        &ctx,
        &mut sess_c,
        "trademgr",
        "InitiateTrade",
        &[PyRep::Integer(char_b)],
        &HashMap::new(),
    )
    .unwrap()
    .unwrap_err();
    assert_eq!(dup_partner_err.code, "TradeAlreadyActive");

    // 4. Abort cleans up both bound trademgr objects and allows new trades
    handle(
        &ctx,
        &mut sess_a,
        "trademgr",
        "Abort",
        &[PyRep::Integer(bid_a)],
        &HashMap::new(),
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        ctx.bound_objects
            .read()
            .unwrap()
            .values()
            .filter(|e| e.service == "trademgr")
            .count(),
        0
    );
    let next_trade = handle(
        &ctx,
        &mut sess_c,
        "trademgr",
        "InitiateTrade",
        &[PyRep::Integer(char_b)],
        &HashMap::new(),
    )
    .unwrap();
    assert!(next_trade.is_ok());
}

#[test]
fn store_manager_enforces_targeted_audience_and_product_omega_without_cat_1() {
    use eve_db::service_domains::quasar_and_social::{StoreOffer, StoreProduct};
    use eve_db::TargetedOfferAdminRecord;

    const ERR_TARGETED_NOT_ELIGIBLE: &str = "TargetedOfferNotEligible";

    let ctx = make_test_context();
    let char_id = 90_000_001_i64;
    let mut session = char_session(char_id, 98_000_001);
    let kw = HashMap::new();

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.clone_grade = 1;
        }
        snap.account_plex_vault.insert(1, 2_000);
        let mut cat = crate::services::quasar_payment_store_types::read_store_catalog(snap);
        cat.products.push(StoreProduct {
            id: 7001,
            name: "30 Days Omega Time".into(),
            description: "Omega license via non-cat-1 offer".into(),
            type_id: 0,
            quantity: 1,
        });
        cat.products.push(StoreProduct {
            id: 7002,
            name: "Promo Ship SKIN".into(),
            description: "Bonus SKIN item".into(),
            type_id: 34_599,
            quantity: 1,
        });
        cat.offers.push(StoreOffer {
            id: 7001,
            name: "Pilot Bundle".into(),
            description: "Non-cat-1 Omega + SKIN".into(),
            category_id: 3,
            price_plex: 250,
            product_ids: vec![7001, 7002],
            targeted: false,
            price_cents: None,
            currency: None,
        });
        cat.offers.push(StoreOffer {
            id: 7002,
            name: "Targeted Promo SKIN".into(),
            description: "Audience-restricted SKIN".into(),
            category_id: 2,
            price_plex: 100,
            product_ids: vec![7002],
            targeted: true,
            price_cents: None,
            currency: None,
        });
        crate::services::quasar_payment_store_types::save_store_catalog(snap, &cat);
        snap.quasar_commerce.targeted_offers.insert(
            "999:7002".into(),
            TargetedOfferAdminRecord {
                user_id: 999,
                catalogue_entry_name: "7002".into(),
                promoted: true,
                retracted: false,
                earliest_reissue_unix: 0,
                partner_code: String::new(),
            },
        );
    });

    // 1. Targeted offer owned by another user is denied cleanly
    let denied = handle(
        &ctx,
        &mut session,
        "storeManager",
        "can_purchase_offer",
        &[PyRep::Integer(7002)],
        &kw,
    )
    .unwrap()
    .unwrap_err();
    assert_eq!(denied.code, ERR_TARGETED_NOT_ELIGIBLE);

    // Promote for user 1 -> allowed; then retract -> denied again
    ctx.db.with_mut_snapshot(|snap| {
        snap.quasar_commerce.targeted_offers.insert(
            "1:7002".into(),
            TargetedOfferAdminRecord {
                user_id: 1,
                catalogue_entry_name: "7002".into(),
                promoted: true,
                retracted: false,
                earliest_reissue_unix: 0,
                partner_code: String::new(),
            },
        );
    });
    assert_eq!(
        handle(
            &ctx,
            &mut session,
            "storeManager",
            "can_purchase_offer",
            &[PyRep::Integer(7002)],
            &kw,
        )
        .unwrap()
        .unwrap(),
        PyRep::Bool(true)
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.quasar_commerce
            .targeted_offers
            .get_mut("1:7002")
            .unwrap()
            .retracted = true;
    });
    let retracted_err = handle(
        &ctx,
        &mut session,
        "storeManager",
        "buy_offer",
        &[PyRep::Integer(7002)],
        &kw,
    )
    .unwrap()
    .unwrap_err();
    assert_eq!(retracted_err.code, ERR_TARGETED_NOT_ELIGIBLE);

    // 2. Non-category-1 Omega + SKIN bundle activates Omega (`ActivatedNotice`) AND delivers SKIN
    let _ = ctx
        .quasar_gateway
        .pending_notices
        .write()
        .unwrap()
        .drain(..)
        .count();
    let buy_omega = handle(
        &ctx,
        &mut session,
        "storeManager",
        "buy_offer",
        &[PyRep::Integer(7001)],
        &kw,
    )
    .unwrap()
    .expect("non-cat-1 Omega bundle purchase must succeed");
    assert_eq!(
        buy_omega.dict_get("plexRemaining").and_then(|v| v.as_i64()),
        Some(1_750)
    );
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.characters.get(&char_id).map(|c| c.clone_grade), Some(2));
        assert!(snap
            .items
            .values()
            .any(|it| it.owner_id == char_id && it.type_id == 34_599));
    });
    assert!(
        ctx.quasar_gateway
            .pending_notices
            .read()
            .unwrap()
            .iter()
            .any(|(scope, any)| {
                matches!(scope, crate::quasar_gateway::NoticeScope::User(1))
                    && any.type_url.contains("ActivatedNotice")
            }),
        "storeManager non-cat-1 Omega bundle must queue ActivatedNotice"
    );
}
