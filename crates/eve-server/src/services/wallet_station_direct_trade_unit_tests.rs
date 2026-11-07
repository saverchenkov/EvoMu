//! Semantic unit tests split from `wallet_services_ext_extra_unit_tests`.

#![forbid(unsafe_code)]

use super::wallet_services_ext_extra_unit_tests::*;
use eve_db::IskCents;
use eve_machonet::SessionState;
use eve_proto::PyRep;
use std::collections::HashMap;

#[test]
fn station_direct_trade_edge_cases_abort_corrupt_singleton_and_reverse_isk() {
    let ctx = test_ctx();
    let char_a = 90_000_001_i64;
    let char_b = 90_000_002_i64;
    let char_c = 90_000_003_i64;
    let st_id = 60_003_760_i64;
    let kw = HashMap::new();

    let ship_a = ctx.db.with_snapshot(|s| s.characters[&char_a].active_ship_id);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ca) = snap.characters.get_mut(&char_a) {
            ca.station_id = Some(st_id);
            ca.balance = IskCents::from_f64_isk(10_000.0);
        }
        let base_ch = snap.characters[&char_a].clone();
        let mut cb = base_ch.clone();
        cb.character_id = char_b;
        cb.user_id = 502;
        cb.name = "Trader B".into();
        cb.station_id = Some(st_id);
        cb.balance = IskCents::from_f64_isk(50_000.0);
        snap.characters.insert(char_b, cb);

        let mut cc = base_ch;
        cc.character_id = char_c;
        cc.user_id = 503;
        cc.name = "Outsider C".into();
        cc.station_id = Some(60_000_001);
        snap.characters.insert(char_c, cc);

        // Items for testing edge cases:
        // 81_001: courier cargo
        // 81_002: singleton with quantity 2 (cannot split)
        // 81_003: stack of 50 owned by B (for partial B -> A transfer)
        put_hangar_item(snap, 81_001, 34, char_a, st_id, 10, false);
        snap.item_custom_names
            .insert(81_001, "courier_cargo:999".into());
        put_hangar_item(snap, 81_002, 587, char_a, st_id, 2, true);
        put_hangar_item(snap, 81_003, 34, char_b, st_id, 50, false);
    });

    let mut sess_a = SessionState {
        user_id: Some(1),
        character_id: Some(char_a),
        corporation_id: Some(98_000_001),
        station_id: Some(st_id),
        ship_id: Some(ship_a),
        ..Default::default()
    };
    let mut sess_b = SessionState {
        user_id: Some(502),
        character_id: Some(char_b),
        corporation_id: Some(98_000_001),
        station_id: Some(st_id),
        ..Default::default()
    };
    let mut sess_c = SessionState {
        user_id: Some(503),
        character_id: Some(char_c),
        corporation_id: Some(98_000_001),
        station_id: Some(60_000_001),
        ..Default::default()
    };

    // 1. InitiateTrade errors: missing arg, CharacterNotFound, PartnerNotInStation, trade_id 0 with no trade
    assert_eq!(
        call_wallet_ext(&ctx, &mut sess_a, "trademgr", "InitiateTrade", &[], &kw)
            .unwrap()
            .unwrap_err()
            .code,
        "InvalidArgument"
    );
    assert_eq!(
        call_wallet_ext(
            &ctx,
            &mut sess_a,
            "trademgr",
            "InitiateTrade",
            &[PyRep::Integer(999_999_999)],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "CharacterNotFound"
    );
    assert_eq!(
        call_wallet_ext(
            &ctx,
            &mut sess_a,
            "trademgr",
            "InitiateTrade",
            &[PyRep::Integer(char_c)],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "PartnerNotInStation"
    );
    assert_eq!(
        call_wallet_ext(&ctx, &mut sess_a, "trademgr", "List", &[PyRep::Integer(0)], &kw)
            .unwrap()
            .unwrap_err()
            .code,
        "TradeNotFound"
    );

    // 2. Initiate valid trade & test trade_id == 0 lookup, AccessDenied by C, Abort
    let init1 = call_wallet_ext(
        &ctx,
        &mut sess_a,
        "trademgr",
        "InitiateTrade",
        &[PyRep::Integer(char_b)],
        &kw,
    )
    .unwrap()
    .unwrap();
    let tid1 = init1.dict_get("tradeContainerID").unwrap().as_i64().unwrap();

    // Lookup via trade_id = 0
    let list_zero = call_wallet_ext(&ctx, &mut sess_a, "trademgr", "List", &[PyRep::Integer(0)], &kw)
        .unwrap()
        .unwrap();
    assert_eq!(
        list_zero.dict_get("tradeContainerID"),
        Some(&PyRep::Integer(tid1))
    );

    // Outsider C rejected with AccessDenied
    assert_eq!(
        call_wallet_ext(&ctx, &mut sess_c, "trademgr", "List", &[PyRep::Integer(tid1)], &kw)
            .unwrap()
            .unwrap_err()
            .code,
        "AccessDenied"
    );

    // Abort closes trade
    let aborted = call_wallet_ext(&ctx, &mut sess_a, "trademgr", "Abort", &[PyRep::Integer(tid1)], &kw)
        .unwrap()
        .unwrap();
    assert_eq!(aborted.dict_get("cancelled"), Some(&PyRep::Bool(true)));

    // 3. Second trade: test AddItem validations (courier cargo, active ship, singleton split, duplicate accumulation, quantity overflow)
    // and B -> A partial item transfer + B -> A net ISK transfer
    let init2 = call_wallet_ext(
        &ctx,
        &mut sess_a,
        "trademgr",
        "InitiateTrade",
        &[PyRep::Integer(char_b)],
        &kw,
    )
    .unwrap()
    .unwrap();
    let tid2 = init2.dict_get("tradeContainerID").unwrap().as_i64().unwrap();

    assert_eq!(
        call_wallet_ext(
            &ctx,
            &mut sess_a,
            "trademgr",
            "AddItem",
            &[PyRep::Integer(tid2), PyRep::Integer(999_999_999)],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "ItemNotFound"
    );
    assert_eq!(
        call_wallet_ext(
            &ctx,
            &mut sess_a,
            "trademgr",
            "AddItem",
            &[PyRep::Integer(tid2), PyRep::Integer(81_001), PyRep::Integer(1)],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidTradeItem"
    );
    assert_eq!(
        call_wallet_ext(
            &ctx,
            &mut sess_a,
            "trademgr",
            "AddItem",
            &[PyRep::Integer(tid2), PyRep::Integer(81_002), PyRep::Integer(0)],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidQuantity"
    );
    assert_eq!(
        call_wallet_ext(
            &ctx,
            &mut sess_a,
            "trademgr",
            "AddItem",
            &[PyRep::Integer(tid2), PyRep::Integer(81_002), PyRep::Integer(1)],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "CannotSplitSingleton"
    );

    // B adds 20 of 81_003, then adds another 15 (= 35 <= 50), then tries to add 20 (= 55 > 50 -> InsufficientQuantity)
    call_wallet_ext(
        &ctx,
        &mut sess_b,
        "trademgr",
        "AddItem",
        &[PyRep::Integer(tid2), PyRep::Integer(81_003), PyRep::Integer(20)],
        &kw,
    )
    .unwrap()
    .unwrap();
    call_wallet_ext(
        &ctx,
        &mut sess_b,
        "trademgr",
        "AddItem",
        &[PyRep::Integer(tid2), PyRep::Integer(81_003), PyRep::Integer(15)],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        call_wallet_ext(
            &ctx,
            &mut sess_b,
            "trademgr",
            "AddItem",
            &[PyRep::Integer(tid2), PyRep::Integer(81_003), PyRep::Integer(20)],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InsufficientQuantity"
    );

    // OfferMoney validation: negative & NotEnoughMoney, then B offers 15,000 ISK to A
    assert_eq!(
        call_wallet_ext(
            &ctx,
            &mut sess_b,
            "trademgr",
            "OfferMoney",
            &[PyRep::Integer(tid2), PyRep::Real(-5.0)],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidArgument"
    );
    assert_eq!(
        call_wallet_ext(
            &ctx,
            &mut sess_a,
            "trademgr",
            "OfferMoney",
            &[PyRep::Integer(tid2), PyRep::Real(999_999.0)],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "NotEnoughMoney"
    );
    call_wallet_ext(
        &ctx,
        &mut sess_b,
        "trademgr",
        "OfferMoney",
        &[PyRep::Integer(tid2), PyRep::Real(15_000.0)],
        &kw,
    )
    .unwrap()
    .unwrap();

    // Complete trade: B transfers 35 of 81_003 and 15,000 ISK to A
    call_wallet_ext(
        &ctx,
        &mut sess_a,
        "trademgr",
        "MakeOffer",
        &[PyRep::Integer(tid2), PyRep::Bool(true)],
        &kw,
    )
    .unwrap()
    .unwrap();
    let done = call_wallet_ext(
        &ctx,
        &mut sess_b,
        "trademgr",
        "MakeOffer",
        &[PyRep::Integer(tid2), PyRep::Bool(true)],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(done.dict_get("completed"), Some(&PyRep::Bool(true)));
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items[&81_003].quantity, 15);
        assert_eq!(
            snap.characters[&char_a].balance,
            IskCents::from_f64_isk(25_000.0)
        );
        assert_eq!(
            snap.characters[&char_b].balance,
            IskCents::from_f64_isk(35_000.0)
        );
    });
}
