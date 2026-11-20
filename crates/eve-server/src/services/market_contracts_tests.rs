//! Unit tests for `market_contracts` (PLEX market, order expiry/modification, fee math, contract helpers).

#![forbid(unsafe_code)]

use super::*;
use crate::router::ServerContext;
use eve_db::{
    InventoryFlag, IskCents, MarketOrderRecord,
};
use eve_machonet::SessionState;
use eve_sde::SdeRegistry;
use std::sync::{Arc, OnceLock};

#[cfg(test)]
#[path = "market_orders_modify_and_auctions_tests.rs"]
mod market_orders_modify_and_auctions_tests;

pub(super) static SHARED_SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();

pub(super) fn test_ctx() -> ServerContext {
    if let Some(sde) = SHARED_SDE.get() {
        ServerContext::with_sde_and_repository(
            Arc::clone(sde),
            Arc::new(eve_db::WorldRepository::with_demo_seed()),
        )
        .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED)
    } else {
        let ctx = ServerContext::new();
        let _ = SHARED_SDE.set(Arc::clone(&ctx.sde));
        ctx
    }
}

pub(super) fn jita_session(char_id: i64, corp_id: i64) -> SessionState {
    let mut s = SessionState::new(9101);
    s.user_id = Some(1);
    s.character_id = Some(char_id);
    s.corporation_id = Some(corp_id);
    s.station_id = Some(JITA_STATION_ID);
    s.solar_system_id = Some(30_000_142);
    s.region_id = Some(THE_FORGE_REGION_ID);
    s
}

#[test]
fn market_price_tick_remote_jumps_sales_tax_and_relist_fee_math() {
    assert!(is_valid_4_sig_fig_price(100.0));
    assert!(is_valid_4_sig_fig_price(1234.0));
    assert!(is_valid_4_sig_fig_price(12340.0));
    assert!(is_valid_4_sig_fig_price(123_400_000.0));
    assert!(is_valid_4_sig_fig_price(0.12));
    assert!(is_valid_4_sig_fig_price(99.99));
    assert!(!is_valid_4_sig_fig_price(12345.0));
    assert!(!is_valid_4_sig_fig_price(1.2345));
    assert!(!is_valid_4_sig_fig_price(100.01));
    assert!(!is_valid_4_sig_fig_price(0.0));
    assert!(!is_valid_4_sig_fig_price(-10.0));
    assert!(!is_valid_4_sig_fig_price(f64::NAN));
    assert!(!is_valid_4_sig_fig_price(f64::INFINITY));
    assert!(!is_valid_4_sig_fig_price(0.001));

    assert_eq!(max_remote_jumps_for_skill_level(0), -1);
    assert_eq!(max_remote_jumps_for_skill_level(1), 0);
    assert_eq!(max_remote_jumps_for_skill_level(2), 5);
    assert_eq!(max_remote_jumps_for_skill_level(3), 10);
    assert_eq!(max_remote_jumps_for_skill_level(4), 20);
    assert_eq!(max_remote_jumps_for_skill_level(5), 32_767);
    assert_eq!(max_remote_jumps_for_skill_level(9), 32_767);

    let gross = IskCents::from_isk(100_000_000);
    let tax0 = compute_sales_tax_cents(gross, 0);
    let tax5 = compute_sales_tax_cents(gross, 5);
    assert_eq!(tax0.0, 7_500_000_00);
    assert!(tax5.0 < tax0.0);
    assert_eq!(tax5.0, 3_375_000_00);

    // Minimum floor of 100 ISK (10_000 cents) on tiny modification
    let min_fee = compute_relist_fee_cents(
        IskCents::from_isk(100),
        IskCents::from_isk(90),
        1,
        0.03,
        5,
    );
    assert_eq!(min_fee, IskCents(10_000));

    // Price increase charges both base relist discount and 100% of incremental delta * broker_rate
    let inc_fee = compute_relist_fee_cents(
        IskCents::from_isk(10_000),
        IskCents::from_isk(20_000),
        100,
        0.02,
        0,
    );
    // new_total = 2_000_000 ISK; base = 2_000_000 * 0.02 * 0.50 = 20_000 ISK
    // inc = 1_000_000 ISK; delta = 1_000_000 * 0.02 = 20_000 ISK => total = 40_000 ISK
    assert_eq!(inc_fee, IskCents::from_isk(40_000));

    // Price decrease with Advanced Broker Relations V (discount_factor = 0.20)
    let dec_fee = compute_relist_fee_cents(
        IskCents::from_isk(20_000),
        IskCents::from_isk(10_000),
        100,
        0.02,
        5,
    );
    // new_total = 1_000_000 ISK; base = 1_000_000 * 0.02 * 0.20 = 4_000 ISK
    assert_eq!(dec_fee, IskCents::from_isk(4_000));
}

#[test]
fn sweep_expired_orders_refunds_buy_escrow_and_returns_sell_items() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let now = 1_750_000_000_u64;
    let expired_issue = now - 2 * 86_400;

    let (corp_id, initial_char_bal, initial_corp_bal, initial_plex) = ctx.db.with_mut_snapshot(|s| {
        let cid = s.characters[&char_id].corporation_id;
        s.corporations.entry(cid).or_insert_with(|| { let mut __corp = eve_db::CorporationRecord::default();
            __corp.corporation_id = cid;
            __corp.ceo_id = char_id;
            __corp.name = "SweepTestCorp".into();
            __corp.ticker = "SWTC".into();
            __corp.set_division_balance(1, IskCents::from_isk(1_000_000));
            __corp });
        let cbal = s.characters[&char_id].balance;
        let corp_bal = s.corporations[&cid].master_wallet_balance();
        let plex_before = s.plex_vault_balance(char_id);

        // 1. Expired personal buy order
        s.market_orders.insert(
            700_001,
            MarketOrderRecord {
                order_id: 700_001,
                owner_id: char_id,
                station_id: JITA_STATION_ID,
                region_id: THE_FORGE_REGION_ID,
                type_id: 34,
                is_bid: true,
                price: IskCents::from_isk(5),
                vol_entered: 1000,
                vol_remaining: 400,
                min_volume: 1,
                is_npc: false,
                escrow: IskCents::from_isk(2000),
            },
        );
        save_market_order_meta(
            s,
            700_001,
            MarketOrderMeta {
                range: -1,
                duration: 1,
                issue_unix: expired_issue,
                order_state: None,
                account_key: None,
                final_vol_remaining: None,
                closed_unix: None,
            },
        );

        // 2. Expired corporate buy order (division 2 -> accountKey 1001)
        s.market_orders.insert(
            700_002,
            MarketOrderRecord {
                order_id: 700_002,
                owner_id: cid,
                station_id: JITA_STATION_ID,
                region_id: THE_FORGE_REGION_ID,
                type_id: 34,
                is_bid: true,
                price: IskCents::from_isk(5),
                vol_entered: 500,
                vol_remaining: 500,
                min_volume: 1,
                is_npc: false,
                escrow: IskCents::from_isk(2500),
            },
        );
        save_market_order_meta(
            s,
            700_002,
            MarketOrderMeta {
                range: -1,
                duration: 1,
                issue_unix: expired_issue,
                order_state: None,
                account_key: Some(1001),
                final_vol_remaining: None,
                closed_unix: None,
            },
        );

        // 3. Expired personal PLEX sell order
        s.market_orders.insert(
            700_003,
            MarketOrderRecord {
                order_id: 700_003,
                owner_id: char_id,
                station_id: JITA_STATION_ID,
                region_id: THE_FORGE_REGION_ID,
                type_id: PLEX_TYPE_ID,
                is_bid: false,
                price: IskCents::from_isk(5_000_000),
                vol_entered: 10,
                vol_remaining: 6,
                min_volume: 1,
                is_npc: false,
                escrow: IskCents(0),
            },
        );
        save_market_order_meta(
            s,
            700_003,
            MarketOrderMeta {
                range: -1,
                duration: 1,
                issue_unix: expired_issue,
                order_state: None,
                account_key: None,
                final_vol_remaining: None,
                closed_unix: None,
            },
        );

        // 4. Expired corporate regular item sell order
        s.market_orders.insert(
            700_004,
            MarketOrderRecord {
                order_id: 700_004,
                owner_id: cid,
                station_id: JITA_STATION_ID,
                region_id: THE_FORGE_REGION_ID,
                type_id: 35,
                is_bid: false,
                price: IskCents::from_isk(20),
                vol_entered: 300,
                vol_remaining: 150,
                min_volume: 1,
                is_npc: false,
                escrow: IskCents(0),
            },
        );
        save_market_order_meta(
            s,
            700_004,
            MarketOrderMeta {
                range: -1,
                duration: 1,
                issue_unix: expired_issue,
                order_state: None,
                account_key: Some(1000),
                final_vol_remaining: None,
                closed_unix: None,
            },
        );

        // 5. Active non-expired personal buy order (must remain active)
        s.market_orders.insert(
            700_005,
            MarketOrderRecord {
                order_id: 700_005,
                owner_id: char_id,
                station_id: JITA_STATION_ID,
                region_id: THE_FORGE_REGION_ID,
                type_id: 34,
                is_bid: true,
                price: IskCents::from_isk(6),
                vol_entered: 100,
                vol_remaining: 100,
                min_volume: 1,
                is_npc: false,
                escrow: IskCents::from_isk(600),
            },
        );
        save_market_order_meta(
            s,
            700_005,
            MarketOrderMeta {
                range: -1,
                duration: 7,
                issue_unix: now - 86_400,
                order_state: None,
                account_key: None,
                final_vol_remaining: None,
                closed_unix: None,
            },
        );

        (cid, cbal, corp_bal, plex_before)
    });

    let swept = ctx.db.with_mut_snapshot(|s| sweep_expired_orders(s, now));
    assert_eq!(swept.len(), 4);

    ctx.db.with_snapshot(|s| {
        assert_eq!(s.characters[&char_id].balance, IskCents(initial_char_bal.0 + 2000_00));
        assert_eq!(s.corporations[&corp_id].master_wallet_balance(), initial_corp_bal);
        assert_eq!(s.corporations[&corp_id].division_balance(2), IskCents(2500_00));
        assert_eq!(s.plex_vault_balance(char_id), initial_plex + 6);
        assert_eq!(s.market_orders[&700_001].vol_remaining, 0);
        assert_eq!(s.market_orders[&700_001].escrow, IskCents(0));
        assert_eq!(s.market_orders[&700_005].vol_remaining, 100);

        let corp_deliv = s
            .items
            .values()
            .find(|i| {
                i.owner_id == corp_id
                    && i.location_id == JITA_STATION_ID
                    && i.flag == InventoryFlag::Deliveries
                    && i.type_id == 35
            })
            .expect("expired corp sell items returned to Deliveries");
        assert_eq!(corp_deliv.quantity, 150);

        let meta1 = load_market_order_meta(s, &s.market_orders[&700_001], now);
        assert_eq!(meta1.order_state, Some(4));
        assert_eq!(meta1.final_vol_remaining, Some(400));
        assert_eq!(meta1.closed_unix, Some(now));
    });
}

#[test]
fn get_system_asks_and_plex_best_omit_expired_orders_without_sweeping() {
    let ctx = test_ctx();
    let now = 1_750_000_000_u64;
    ctx.set_clock(Arc::new(eve_db::VirtualServerClock::with_fixed_base(now)));
    let char_id = 90_000_001_i64;
    let mut sess = jita_session(char_id, 1_000_009);
    let kw = std::collections::HashMap::new();

    ctx.db.with_mut_snapshot(|s| {
        s.market_orders.clear();
        s.market_orders.insert(
            710_001,
            MarketOrderRecord {
                order_id: 710_001,
                owner_id: char_id,
                station_id: JITA_STATION_ID,
                region_id: THE_FORGE_REGION_ID,
                type_id: 34,
                is_bid: false,
                price: IskCents::from_isk(10),
                vol_entered: 50,
                vol_remaining: 50,
                min_volume: 1,
                is_npc: false,
                escrow: IskCents(0),
            },
        );
        save_market_order_meta(
            s,
            710_001,
            MarketOrderMeta {
                range: -1,
                duration: 1,
                issue_unix: now - 2 * 86_400,
                order_state: None,
                account_key: None,
                final_vol_remaining: None,
                closed_unix: None,
            },
        );
    });

    let sys_asks = handle(
        &ctx,
        &mut sess,
        "marketProxy",
        "GetSystemAsks",
        &[eve_proto::PyRep::Integer(30_000_142)],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert!(sys_asks.cindexed_rowset_entries().unwrap().is_empty());
    ctx.db.with_snapshot(|s| {
        assert_eq!(s.market_orders[&710_001].vol_remaining, 50);
    });

    // Seed expired PLEX sell and buy orders and verify GetPlexBest omits expired asks on read without mutating DB
    ctx.db.with_mut_snapshot(|s| {
        s.market_orders.insert(
            710_002,
            MarketOrderRecord {
                order_id: 710_002,
                owner_id: char_id,
                station_id: JITA_STATION_ID,
                region_id: THE_FORGE_REGION_ID,
                type_id: PLEX_TYPE_ID,
                is_bid: false,
                price: IskCents::from_isk(5_000_000),
                vol_entered: 2,
                vol_remaining: 2,
                min_volume: 1,
                is_npc: false,
                escrow: IskCents(0),
            },
        );
        save_market_order_meta(
            s,
            710_002,
            MarketOrderMeta {
                range: -1,
                duration: 1,
                issue_unix: now - 2 * 86_400,
                order_state: None,
                account_key: None,
                final_vol_remaining: None,
                closed_unix: None,
            },
        );
    });
    let plex_best = handle(&ctx, &mut sess, "marketProxy", "GetPlexBest", &[], &kw)
        .unwrap()
        .unwrap();
    assert!(plex_best.is_cindexed_rowset());
    assert!(
        !plex_best
            .cindexed_rowset_entries()
            .unwrap()
            .iter()
            .any(|(k, _)| k == &PyRep::Integer(PLEX_TYPE_ID as i64))
    );
    ctx.db.with_snapshot(|s| {
        assert_eq!(s.market_orders[&710_002].vol_remaining, 2);
    });

    // Explicit sweep cleans up both expired orders.
    assert_eq!(sweep_expired_orders_and_notify(&ctx, now), 2);
    ctx.db.with_snapshot(|s| {
        assert_eq!(s.market_orders[&710_001].vol_remaining, 0);
        assert_eq!(s.market_orders[&710_002].vol_remaining, 0);
    });
}

#[test]
fn startup_check_and_contract_container_rbac() {
    let ctx = test_ctx();
    let kw = std::collections::HashMap::new();
    let corp_id = 98_000_811;
    let ceo_id = 2_000_811;
    let q1_id = 2_000_812;
    let q2_id = 2_000_813;
    let plain_id = 2_000_814;
    let npc_corp_id = 1_000_009;
    let corp_cont_id = 9_100_811;
    let corp_item_id = 9_100_812;
    let npc_cont_id = 9_100_813;

    // 1. Unauthenticated StartupCheck must fail closed without mutating market_orders.
    // Ensure Sleeper orders start unseeded even when `test_ctx()` returns the first `ServerContext::new()`.
    ctx.db
        .with_mut_snapshot(|s| s.market_orders.retain(|_, o| !is_permanent_npc_sleeper_buy_order(o)));
    let before_orders = ctx.db.with_snapshot(|s| s.market_orders.len());
    let mut unauth_sess = SessionState::new(9199);
    let unauth_err = handle(&ctx, &mut unauth_sess, "marketProxy", "StartupCheck", &[], &kw)
        .unwrap()
        .unwrap_err();
    assert!(!unauth_err.code.is_empty());
    assert_eq!(ctx.db.with_snapshot(|s| s.market_orders.len()), before_orders);

    // 2. Authenticated StartupCheck seeds Sleeper orders on first call, then runs read-pure on subsequent calls.
    let mut sess_q2 = jita_session(q2_id, corp_id);
    let first = handle(&ctx, &mut sess_q2, "marketProxy", "StartupCheck", &[], &kw)
        .unwrap()
        .unwrap();
    assert_eq!(first.dict_get("status"), Some(&PyRep::String("ok".into())));
    let after_first_orders = ctx.db.with_snapshot(|s| s.market_orders.len());
    let next_eid_after_first = ctx.db.with_snapshot(|s| s.next_entity_id);
    assert!(after_first_orders > before_orders);

    let second = handle(&ctx, &mut sess_q2, "marketProxy", "StartupCheck", &[], &kw)
        .unwrap()
        .unwrap();
    assert_eq!(second.dict_get("status"), Some(&PyRep::String("ok".into())));
    assert_eq!(second.dict_get("expiredOrdersSwept"), Some(&PyRep::Integer(0)));
    assert_eq!(ctx.db.with_snapshot(|s| s.next_entity_id), next_eid_after_first);

    // 3. contractProxy.{GetItemsInContainer, GetNumItemsInContainers} enforce container ownership & hangar query roles.
    ctx.db.with_mut_snapshot(|s| {
        let mut corp = eve_db::CorporationRecord::default();
        corp.corporation_id = corp_id;
        corp.name = "ContainerCorp".into();
        corp.ticker = "CCNT".into();
        corp.ceo_id = ceo_id;
        corp.members.extend([ceo_id, q1_id, q2_id, plain_id]);
        corp.member_roles
            .insert(q1_id, eve_db::corp_roles::ROLE_HANGAR_CAN_QUERY[0]);
        corp.member_roles
            .insert(q2_id, eve_db::corp_roles::ROLE_HANGAR_CAN_QUERY[1]);
        s.corporations.insert(corp_id, corp);
        let mut q1_char = eve_db::CharacterRecord::default();
        q1_char.character_id = q1_id;
        q1_char.name = "QueryOne".into();
        q1_char.corporation_id = corp_id;
        s.characters.insert(q1_id, q1_char);

        s.items.insert(
            corp_cont_id,
            InventoryItemRecord {
                item_id: corp_cont_id,
                type_id: 17_366,
                owner_id: corp_id,
                location_id: JITA_STATION_ID,
                flag: InventoryFlag::CorpSAG2,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        s.items.insert(
            corp_item_id,
            InventoryItemRecord {
                item_id: corp_item_id,
                type_id: 34,
                owner_id: corp_id,
                location_id: corp_cont_id,
                flag: InventoryFlag::Cargo,
                quantity: 50,
                singleton: false,
                ..Default::default()
            },
        );
        s.items.insert(
            npc_cont_id,
            InventoryItemRecord {
                item_id: npc_cont_id,
                type_id: 17_366,
                owner_id: npc_corp_id,
                location_id: JITA_STATION_ID,
                flag: InventoryFlag::CorpSAG1,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });

    let mut sess_plain = jita_session(plain_id, corp_id);
    let mut sess_q1 = jita_session(q1_id, corp_id);
    let mut sess_npc = jita_session(plain_id, npc_corp_id);

    // Non-corp unpopulated sentinel/contract probe (client_contract_ratchet) returns empty shape.
    assert_eq!(
        handle(
            &ctx,
            &mut sess_plain,
            "contractProxy",
            "GetItemsInContainer",
            &[PyRep::Integer(JITA_STATION_ID), PyRep::Integer(1), PyRep::Bool(false), PyRep::Integer(4)],
            &kw,
        )
        .unwrap()
        .unwrap(),
        PyRep::List(vec![])
    );
    assert_eq!(
        handle(
            &ctx,
            &mut sess_plain,
            "contractProxy",
            "GetNumItemsInContainers",
            &[PyRep::Integer(JITA_STATION_ID), PyRep::Integer(1), PyRep::Bool(false), PyRep::Integer(4)],
            &kw,
        )
        .unwrap()
        .unwrap(),
        PyRep::Dict(vec![])
    );

    // forCorp=true enforces division query role & non-NPC corp even on unpopulated container IDs.
    let sag2 = InventoryFlag::CorpSAG2 as i64;
    for s in [&mut sess_plain, &mut sess_q1, &mut sess_npc] {
        assert_eq!(
            handle(
                &ctx,
                s,
                "contractProxy",
                "GetItemsInContainer",
                &[PyRep::Integer(JITA_STATION_ID), PyRep::Integer(99_888_777), PyRep::Bool(true), PyRep::Integer(sag2)],
                &kw,
            )
            .unwrap()
            .unwrap_err()
            .code,
            "CorpRoleDenied"
        );
    }
    assert_eq!(
        handle(
            &ctx,
            &mut sess_q2,
            "contractProxy",
            "GetItemsInContainer",
            &[PyRep::Integer(JITA_STATION_ID), PyRep::Integer(99_888_777), PyRep::Bool(true), PyRep::Integer(sag2)],
            &kw,
        )
        .unwrap()
        .unwrap(),
        PyRep::List(vec![])
    );

    // Foreign character / corp / NPC corp IDs passed directly as containerID fail closed.
    assert_eq!(
        handle(&ctx, &mut sess_plain, "contractProxy", "GetItemsInContainer", &[PyRep::Integer(corp_id)], &kw)
            .unwrap()
            .unwrap_err()
            .code,
        "CorpRoleDenied"
    );
    assert_eq!(
        handle(&ctx, &mut sess_plain, "contractProxy", "GetNumItemsInContainers", &[PyRep::Integer(npc_corp_id)], &kw)
            .unwrap()
            .unwrap_err()
            .code,
        "CorpRoleDenied"
    );
    assert_eq!(
        handle(&ctx, &mut sess_plain, "contractProxy", "GetItemsInContainer", &[PyRep::Integer(q1_id)], &kw)
            .unwrap()
            .unwrap_err()
            .code,
        "AccessDenied"
    );

    // Roleless member & wrong-division member are denied CorpRoleDenied.
    assert_eq!(
        handle(&ctx, &mut sess_plain, "contractProxy", "GetItemsInContainer", &[PyRep::Integer(corp_cont_id)], &kw)
            .unwrap()
            .unwrap_err()
            .code,
        "CorpRoleDenied"
    );
    assert_eq!(
        handle(&ctx, &mut sess_q1, "contractProxy", "GetNumItemsInContainers", &[PyRep::List(vec![PyRep::Integer(corp_cont_id)])], &kw)
            .unwrap()
            .unwrap_err()
            .code,
        "CorpRoleDenied"
    );
    assert_eq!(
        handle(&ctx, &mut sess_npc, "contractProxy", "GetItemsInContainer", &[PyRep::Integer(npc_cont_id)], &kw)
            .unwrap()
            .unwrap_err()
            .code,
        "CorpRoleDenied"
    );

    // Matching division hangar query role (CorpSAG2 -> ROLE_HANGAR_CAN_QUERY[1]) succeeds.
    let items_rep = handle(&ctx, &mut sess_q2, "contractProxy", "GetItemsInContainer", &[PyRep::Integer(corp_cont_id)], &kw)
        .unwrap()
        .unwrap();
    let PyRep::List(rows) = items_rep else { panic!("expected List from GetItemsInContainer"); };
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].dict_get("itemID"), Some(&PyRep::Integer(corp_item_id)));

    let counts_rep = handle(
        &ctx,
        &mut sess_q2,
        "contractProxy",
        "GetNumItemsInContainers",
        &[PyRep::List(vec![PyRep::Integer(corp_cont_id)])],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        counts_rep,
        PyRep::Dict(vec![(PyRep::Integer(corp_cont_id), PyRep::Integer(1))])
    );

    // 4. Corporate office (type_id == 27, flag == Hangar) & multi-hop nested containers cannot leak restricted CorpSAG2..7 items.
    let office_id = 9_100_820;
    let office_sag1_item_id = 9_100_821;
    let office_sag2_item_id = 9_100_822;
    let sag2_outer_box_id = 9_100_823;
    let sag2_nested_box_id = 9_100_824;
    let sag2_nested_item_id = 9_100_825;
    let sag2_only_office_id = 9_100_826;
    let sag2_only_item_id = 9_100_827;
    ctx.db.with_mut_snapshot(|s| {
        s.items.insert(
            office_id,
            InventoryItemRecord {
                item_id: office_id,
                type_id: 27,
                owner_id: corp_id,
                location_id: JITA_STATION_ID,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        s.items.insert(
            office_sag1_item_id,
            InventoryItemRecord {
                item_id: office_sag1_item_id,
                type_id: 34,
                owner_id: corp_id,
                location_id: office_id,
                flag: InventoryFlag::CorpSAG1,
                quantity: 10,
                singleton: false,
                ..Default::default()
            },
        );
        s.items.insert(
            office_sag2_item_id,
            InventoryItemRecord {
                item_id: office_sag2_item_id,
                type_id: 35,
                owner_id: corp_id,
                location_id: office_id,
                flag: InventoryFlag::CorpSAG2,
                quantity: 20,
                singleton: false,
                ..Default::default()
            },
        );
        s.items.insert(
            sag2_outer_box_id,
            InventoryItemRecord {
                item_id: sag2_outer_box_id,
                type_id: 17_366,
                owner_id: corp_id,
                location_id: office_id,
                flag: InventoryFlag::CorpSAG2,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        s.items.insert(
            sag2_nested_box_id,
            InventoryItemRecord {
                item_id: sag2_nested_box_id,
                type_id: 17_366,
                owner_id: corp_id,
                location_id: sag2_outer_box_id,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        s.items.insert(
            sag2_nested_item_id,
            InventoryItemRecord {
                item_id: sag2_nested_item_id,
                type_id: 36,
                owner_id: corp_id,
                location_id: sag2_nested_box_id,
                flag: InventoryFlag::None,
                quantity: 30,
                singleton: false,
                ..Default::default()
            },
        );
        s.items.insert(
            sag2_only_office_id,
            InventoryItemRecord {
                item_id: sag2_only_office_id,
                type_id: 27,
                owner_id: corp_id,
                location_id: JITA_STATION_ID,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        s.items.insert(
            sag2_only_item_id,
            InventoryItemRecord {
                item_id: sag2_only_item_id,
                type_id: 37,
                owner_id: corp_id,
                location_id: sag2_only_office_id,
                flag: InventoryFlag::CorpSAG2,
                quantity: 40,
                singleton: false,
                ..Default::default()
            },
        );
    });

    // q1 querying the mixed office sees/counts ONLY CorpSAG1 items (never CorpSAG2 items/boxes).
    let q1_office_items = handle(
        &ctx,
        &mut sess_q1,
        "contractProxy",
        "GetItemsInContainer",
        &[PyRep::Integer(JITA_STATION_ID), PyRep::Integer(office_id), PyRep::Bool(true), PyRep::Integer(4)],
        &kw,
    )
    .unwrap()
    .unwrap();
    let PyRep::List(q1_office_rows) = q1_office_items else {
        panic!("expected List from office GetItemsInContainer");
    };
    assert_eq!(q1_office_rows.len(), 1);
    assert_eq!(q1_office_rows[0].dict_get("itemID"), Some(&PyRep::Integer(office_sag1_item_id)));

    let q1_office_counts = handle(
        &ctx,
        &mut sess_q1,
        "contractProxy",
        "GetNumItemsInContainers",
        &[
            PyRep::Integer(JITA_STATION_ID),
            PyRep::List(vec![PyRep::Integer(office_id)]),
            PyRep::Bool(true),
            PyRep::Integer(4),
        ],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        q1_office_counts,
        PyRep::Dict(vec![(PyRep::Integer(office_id), PyRep::Integer(1))])
    );

    // Unflagged forCorp=true probe on missing container is denied to single-division member (no catch-all else).
    assert_eq!(
        handle(
            &ctx,
            &mut sess_q1,
            "contractProxy",
            "GetItemsInContainer",
            &[PyRep::Integer(JITA_STATION_ID), PyRep::Integer(99_888_777), PyRep::Bool(true), PyRep::Integer(4)],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "CorpRoleDenied"
    );

    // Office holding only CorpSAG2 items denies q1 when queried without CorpSAG1 filter.
    assert_eq!(
        handle(
            &ctx,
            &mut sess_q1,
            "contractProxy",
            "GetItemsInContainer",
            &[PyRep::Integer(JITA_STATION_ID), PyRep::Integer(sag2_only_office_id), PyRep::Bool(true), PyRep::Integer(4)],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "CorpRoleDenied"
    );

    // Nested box (Cargo) inside CorpSAG2 box inside corp office denies q1 across all ancestor hops.
    assert_eq!(
        handle(&ctx, &mut sess_q1, "contractProxy", "GetItemsInContainer", &[PyRep::Integer(sag2_nested_box_id)], &kw)
            .unwrap()
            .unwrap_err()
            .code,
        "CorpRoleDenied"
    );
    assert_eq!(
        handle(
            &ctx,
            &mut sess_q1,
            "contractProxy",
            "GetNumItemsInContainers",
            &[PyRep::List(vec![PyRep::Integer(sag2_nested_box_id)])],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "CorpRoleDenied"
    );

    // q2 sees nested item & count inside CorpSAG2 box, and sees its CorpSAG2 office items when filtered.
    let q2_nested = handle(&ctx, &mut sess_q2, "contractProxy", "GetItemsInContainer", &[PyRep::Integer(sag2_nested_box_id)], &kw)
        .unwrap()
        .unwrap();
    let PyRep::List(q2_nested_rows) = q2_nested else {
        panic!("expected List from nested GetItemsInContainer");
    };
    assert_eq!(q2_nested_rows.len(), 1);
    assert_eq!(q2_nested_rows[0].dict_get("itemID"), Some(&PyRep::Integer(sag2_nested_item_id)));
    let q2_nested_counts = handle(
        &ctx,
        &mut sess_q2,
        "contractProxy",
        "GetNumItemsInContainers",
        &[PyRep::List(vec![PyRep::Integer(sag2_nested_box_id)])],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        q2_nested_counts,
        PyRep::Dict(vec![(PyRep::Integer(sag2_nested_box_id), PyRep::Integer(1))])
    );
}

#[test]
fn get_num_items_in_containers_rejects_mixed_invalid_ids_and_checks_db_first_and_members() {
    let ctx = test_ctx();
    let kw = HashMap::new();
    let corp_id = 98_888_810_i64;
    let other_corp_id = 98_888_811_i64;
    let ceo_id = 91_888_810_i64;
    let valid_member_id = 91_888_811_i64;
    let omitted_id = 91_888_812_i64;
    let stale_id = 91_888_813_i64;
    let corp_office_id = 88_888_810_i64;
    let corp_box_id = 88_888_811_i64;
    let inner_item_id = 88_888_812_i64;

    ctx.db.with_mut_snapshot(|s| {
        let mut corp = eve_db::CorporationRecord::default();
        corp.corporation_id = corp_id;
        corp.name = "Container Guard Corp".into();
        corp.ticker = "CGRD".into();
        corp.ceo_id = ceo_id;
        corp.members = std::collections::HashSet::from([ceo_id, valid_member_id]);
        corp.member_roles
            .insert(valid_member_id, eve_db::corp_roles::ROLE_HANGAR_CAN_QUERY[1]);
        corp.member_roles
            .insert(omitted_id, eve_db::corp_roles::ROLE_HANGAR_CAN_QUERY[1]);
        corp.ensure_wallet_divisions();
        s.corporations.insert(corp_id, corp);

        for (cid, coid) in [
            (valid_member_id, corp_id),
            (omitted_id, corp_id),
            (stale_id, other_corp_id),
        ] {
            s.characters.insert(
                cid,
                eve_db::CharacterRecord {
                    character_id: cid,
                    corporation_id: coid,
                    ..Default::default()
                },
            );
        }

        s.items.insert(
            corp_office_id,
            InventoryItemRecord {
                item_id: corp_office_id,
                type_id: 27,
                owner_id: corp_id,
                location_id: JITA_STATION_ID,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        s.items.insert(
            corp_box_id,
            InventoryItemRecord {
                item_id: corp_box_id,
                type_id: 3293,
                owner_id: corp_id,
                location_id: corp_office_id,
                flag: InventoryFlag::CorpSAG2,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        s.items.insert(
            inner_item_id,
            InventoryItemRecord {
                item_id: inner_item_id,
                type_id: 34,
                owner_id: corp_id,
                location_id: corp_box_id,
                flag: InventoryFlag::Cargo,
                quantity: 50,
                singleton: false,
                ..Default::default()
            },
        );
    });

    let mut valid_sess = jita_session(valid_member_id, corp_id);
    assert_eq!(
        handle(
            &ctx,
            &mut valid_sess,
            "contractProxy",
            "GetNumItemsInContainers",
            &[PyRep::List(vec![PyRep::Integer(corp_box_id)])],
            &kw,
        )
        .unwrap()
        .unwrap(),
        PyRep::Dict(vec![(PyRep::Integer(corp_box_id), PyRep::Integer(1))])
    );

    // Mixed non-integer (including Bool), empty, or non-positive elements must fail with InvalidContainer rather than dropping or coercing bad items.
    for bad_arg in [
        PyRep::List(vec![
            PyRep::Integer(corp_box_id),
            PyRep::String("bad".into()),
        ]),
        PyRep::Tuple(vec![PyRep::Integer(corp_box_id), PyRep::None]),
        PyRep::List(vec![PyRep::Bool(true)]),
        PyRep::Tuple(vec![PyRep::Integer(corp_box_id), PyRep::Bool(true)]),
        PyRep::List(vec![PyRep::Integer(corp_box_id), PyRep::Integer(0)]),
        PyRep::List(vec![PyRep::Integer(corp_box_id), PyRep::Integer(-1)]),
        PyRep::List(vec![]),
    ] {
        assert_eq!(
            handle(
                &ctx,
                &mut valid_sess,
                "contractProxy",
                "GetNumItemsInContainers",
                &[bad_arg],
                &kw,
            )
            .unwrap()
            .unwrap_err()
            .code,
            "InvalidContainer"
        );
    }

    // Omitted from non-empty corp.members and stale session.corporation_id are both denied.
    let mut omitted_sess = jita_session(omitted_id, corp_id);
    omitted_sess.corp_role = eve_db::corp_roles::ROLE_HANGAR_CAN_QUERY[1];
    assert_eq!(
        handle(
            &ctx,
            &mut omitted_sess,
            "contractProxy",
            "GetItemsInContainer",
            &[PyRep::Integer(corp_box_id)],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "CorpRoleDenied"
    );

    let mut stale_sess = jita_session(stale_id, corp_id);
    stale_sess.corp_role = eve_db::corp_roles::ROLE_HANGAR_CAN_QUERY[1];
    assert_eq!(
        handle(
            &ctx,
            &mut stale_sess,
            "contractProxy",
            "GetNumItemsInContainers",
            &[PyRep::List(vec![PyRep::Integer(corp_box_id)])],
            &kw,
        )
        .unwrap()
        .unwrap(),
        PyRep::Dict(vec![])
    );
    assert_eq!(
        handle(
            &ctx,
            &mut stale_sess,
            "contractProxy",
            "GetItemsInContainer",
            &[
                PyRep::Integer(JITA_STATION_ID),
                PyRep::Integer(corp_box_id),
                PyRep::Bool(true),
                PyRep::Integer(InventoryFlag::CorpSAG2 as i64),
            ],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "CorporationNotFound"
    );
}

#[test]
fn contract_container_corp_access_ignores_stale_session_corp_role() {
    let ctx = test_ctx();
    let kw = HashMap::new();
    let corp_id = 98_300_401_i64;
    let ceo_id = 90_300_401_i64;
    let plain_member_id = 90_300_402_i64;
    let corp_office_id = 920_300_401_i64;
    let corp_box_id = 920_300_402_i64;
    let inner_item_id = 920_300_403_i64;

    ctx.db.with_mut_snapshot(|s| {
        let mut corp = eve_db::CorporationRecord::default();
        corp.corporation_id = corp_id;
        corp.name = "Contract Role Corp".into();
        corp.ticker = "CRC".into();
        corp.ceo_id = ceo_id;
        corp.members.insert(ceo_id);
        corp.members.insert(plain_member_id);
        s.corporations.insert(corp_id, corp);
        s.characters.insert(
            plain_member_id,
            eve_db::CharacterRecord {
                character_id: plain_member_id,
                corporation_id: corp_id,
                ..Default::default()
            },
        );
        s.items.insert(
            corp_office_id,
            InventoryItemRecord {
                item_id: corp_office_id,
                type_id: 27,
                owner_id: corp_id,
                location_id: JITA_STATION_ID,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        s.items.insert(
            corp_box_id,
            InventoryItemRecord {
                item_id: corp_box_id,
                type_id: 3293,
                owner_id: corp_id,
                location_id: corp_office_id,
                flag: InventoryFlag::CorpSAG2,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        s.items.insert(
            inner_item_id,
            InventoryItemRecord {
                item_id: inner_item_id,
                type_id: 34,
                owner_id: corp_id,
                location_id: corp_box_id,
                flag: InventoryFlag::Cargo,
                quantity: 25,
                singleton: false,
                ..Default::default()
            },
        );
    });

    // Member of registered corp without DB query/director roles cannot use stale session.corp_role.
    let mut plain_sess = jita_session(plain_member_id, corp_id);
    plain_sess.corp_role =
        eve_db::corp_roles::ROLE_DIRECTOR | eve_db::corp_roles::ROLE_HANGAR_CAN_QUERY[1];
    assert_eq!(
        handle(
            &ctx,
            &mut plain_sess,
            "contractProxy",
            "GetNumItemsInContainers",
            &[PyRep::List(vec![PyRep::Integer(corp_box_id)])],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "CorpRoleDenied"
    );
    assert_eq!(
        handle(
            &ctx,
            &mut plain_sess,
            "contractProxy",
            "GetItemsInContainer",
            &[
                PyRep::Integer(JITA_STATION_ID),
                PyRep::Integer(corp_box_id),
                PyRep::Bool(true),
                PyRep::Integer(InventoryFlag::CorpSAG2 as i64),
            ],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "CorpRoleDenied"
    );

    // Grant DB ROLE_HANGAR_CAN_QUERY[1] while clearing session.corp_role -> both queries succeed via DB roles alone.
    ctx.db.with_mut_snapshot(|s| {
        s.corporations
            .get_mut(&corp_id)
            .unwrap()
            .member_roles
            .insert(plain_member_id, eve_db::corp_roles::ROLE_HANGAR_CAN_QUERY[1]);
    });
    plain_sess.corp_role = 0;
    assert_eq!(
        handle(
            &ctx,
            &mut plain_sess,
            "contractProxy",
            "GetNumItemsInContainers",
            &[PyRep::List(vec![PyRep::Integer(corp_box_id)])],
            &kw,
        )
        .unwrap()
        .unwrap(),
        PyRep::Dict(vec![(PyRep::Integer(corp_box_id), PyRep::Integer(1))])
    );
}

#[test]
fn plex_history_rpcs_aggregate_globally_and_read_pure_across_regions() {
    use eve_db::MarketHistoryDayRecord;

    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let corp_id = ctx.db.with_snapshot(|s| s.characters[&char_id].corporation_id);
    let mut sess = jita_session(char_id, corp_id);
    let kw = std::collections::HashMap::new();

    let now_unix = ctx.now_unix();
    let old_date = eve_db::ledger::unix_secs_to_date_str(now_unix.saturating_sub(86_400));
    let today_date = eve_db::ledger::unix_secs_to_date_str(now_unix);

    ctx.db.with_mut_snapshot(|s| {
        s.market_orders.retain(|_, o| o.is_npc || o.vol_remaining <= 0);
        s.market_history.clear();
        s.market_transactions.clear();
        s.market_history.insert(
            (THE_FORGE_REGION_ID, PLEX_TYPE_ID),
            vec![
                MarketHistoryDayRecord {
                    date: old_date.clone(),
                    lowest: 4_600_000.0,
                    highest: 4_800_000.0,
                    average: 4_700_000.0,
                    volume: 20,
                    order_count: 1,
                    open_price: None,
                    close_price: None,
                },
                MarketHistoryDayRecord {
                    date: today_date.clone(),
                    lowest: 5_000_000.0,
                    highest: 5_200_000.0,
                    average: 5_100_000.0,
                    volume: 30,
                    order_count: 2,
                    open_price: None,
                    close_price: None,
                },
            ],
        );
        s.market_history.insert(
            (10_000_043, PLEX_TYPE_ID),
            vec![
                MarketHistoryDayRecord {
                    date: old_date.clone(),
                    lowest: 4_500_000.0,
                    highest: 4_700_000.0,
                    average: 4_600_000.0,
                    volume: 20,
                    order_count: 1,
                    open_price: None,
                    close_price: None,
                },
                MarketHistoryDayRecord {
                    date: today_date.clone(),
                    lowest: 4_700_000.0,
                    highest: 4_900_000.0,
                    average: 4_800_000.0,
                    volume: 10,
                    order_count: 1,
                    open_price: None,
                    close_price: None,
                },
            ],
        );
        s.market_history.insert(
            (THE_FORGE_REGION_ID, 34),
            vec![MarketHistoryDayRecord {
                date: old_date.clone(),
                lowest: 5.0,
                highest: 5.0,
                average: 5.0,
                volume: 100,
                order_count: 1,
                open_price: None,
                close_price: None,
            }],
        );
    });

    let next_eid_before = ctx.db.with_snapshot(|s| s.next_entity_id);
    for region_opt in [Some(10_000_032_u32), None] {
        sess.region_id = region_opt;
        let hist_tuple_res = handle(&ctx, &mut sess, "marketProxy", "GetPlexHistory", &[], &kw)
            .unwrap()
            .unwrap();
        let PyRep::Dict(entries) = hist_tuple_res else {
            panic!("GetPlexHistory must return Dict");
        };
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].0, PyRep::Integer(44_992));
        let PyRep::Tuple(parts) = &entries[0].1 else {
            panic!("GetPlexHistory value must be (old, new) Tuple");
        };
        assert_eq!(parts.len(), 2);
        let tuple_old_rows = parts[0].crowset_rows().expect("old crowset");
        let tuple_new_rows = parts[1].crowset_rows().expect("new crowset");
        assert_eq!(tuple_old_rows.len(), 1);
        assert_eq!(tuple_new_rows.len(), 1);

        let old_res = handle(&ctx, &mut sess, "marketProxy", "GetPlexOldPriceHistory", &[], &kw)
            .unwrap()
            .unwrap();
        assert!(old_res.is_crowset());
        let old_rows = old_res.crowset_rows().expect("old rows");
        assert_eq!(old_rows.len(), 1);
        assert_eq!(old_rows[0].dbrow_get("lowPrice"), Some(&PyRep::Real(4_500_000.0)));
        assert_eq!(old_rows[0].dbrow_get("highPrice"), Some(&PyRep::Real(4_800_000.0)));
        assert!((old_rows[0].dbrow_get("avgPrice").and_then(PyRep::as_f64).unwrap() - 4_650_000.0).abs() < 1e-4);
        assert_eq!(old_rows[0].dbrow_get("volume"), Some(&PyRep::Integer(40)));
        assert_eq!(old_rows[0].dbrow_get("orders"), Some(&PyRep::Integer(2)));

        let new_res = handle(&ctx, &mut sess, "marketProxy", "GetPlexNewPriceHistory", &[], &kw)
            .unwrap()
            .unwrap();
        assert!(new_res.is_crowset());
        let new_rows = new_res.crowset_rows().expect("new rows");
        assert_eq!(new_rows.len(), 1);
        assert_eq!(new_rows[0].dbrow_get("lowPrice"), Some(&PyRep::Real(4_700_000.0)));
        assert_eq!(new_rows[0].dbrow_get("highPrice"), Some(&PyRep::Real(5_200_000.0)));
        let avg = new_rows[0].dbrow_get("avgPrice").and_then(PyRep::as_f64).unwrap();
        assert!((avg - 5_025_000.0).abs() < 1e-4);
        assert_eq!(new_rows[0].dbrow_get("volume"), Some(&PyRep::Integer(40)));
        assert_eq!(new_rows[0].dbrow_get("orders"), Some(&PyRep::Integer(3)));

        assert_ne!(
            old_rows[0].dbrow_get("historyDate"),
            new_rows[0].dbrow_get("historyDate"),
            "GetPlexOldPriceHistory and GetPlexNewPriceHistory must be strictly date-disjoint"
        );
        assert_eq!(tuple_old_rows, old_rows);
        assert_eq!(tuple_new_rows, new_rows);
    }
    assert_eq!(ctx.db.with_snapshot(|s| s.next_entity_id), next_eid_before);

    // GetHistoryForManyTypeIDs from Sinq Laison (10_000_032) returns global PLEX history while keeping Tritanium (34) regional
    sess.region_id = Some(10_000_032);
    let many = handle(
        &ctx,
        &mut sess,
        "marketProxy",
        "GetHistoryForManyTypeIDs",
        &[PyRep::List(vec![
            PyRep::Integer(PLEX_TYPE_ID as i64),
            PyRep::Integer(34),
        ])],
        &kw,
    )
    .unwrap()
    .unwrap();
    let PyRep::Dict(entries) = many else {
        panic!("expected Dict from GetHistoryForManyTypeIDs");
    };
    let plex_rows = entries
        .iter()
        .find(|(k, _)| k.as_i64() == Some(PLEX_TYPE_ID as i64))
        .and_then(|(_, v)| v.crowset_rows())
        .expect("PLEX global history rows");
    assert_eq!(plex_rows.len(), 2);
    assert_eq!(plex_rows[0].dbrow_get("volume"), Some(&PyRep::Integer(40)));
    assert_eq!(plex_rows[1].dbrow_get("volume"), Some(&PyRep::Integer(40)));
    let trit_rows = entries
        .iter()
        .find(|(k, _)| k.as_i64() == Some(34))
        .and_then(|(_, v)| v.crowset_rows())
        .expect("Tritanium regional history rows");
    assert!(trit_rows.is_empty());
}

#[tokio::test]
async fn dispatch_rpc_market_read_getters_stay_pure_and_omit_expired_orders() {
    use crate::services::market_contracts::PLEX_TYPE_ID;
    use crate::services::market_trading::{save_market_order_meta, MarketOrderMeta};
    use eve_db::{CorporationRecord, IskCents, MarketOrderRecord};

    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let corp_id = ctx.db.with_snapshot(|s| s.characters[&char_id].corporation_id);
    let now = ctx.now_unix();
    let kw = HashMap::new();

    let mut sess = SessionState::new(77);
    ctx.dispatch_rpc(
        &mut sess,
        "authentication",
        None,
        "Login",
        &[PyRep::String("demo".into()), PyRep::String("demo-secret".into())],
        &kw,
        None,
    )
    .await
    .expect("Login");
    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID");

    let active_trit_ask = 9_610_001_i64;
    let expired_trit_bid = 9_610_002_i64;
    let expired_corp_bid = 9_610_003_i64;
    let active_plex_ask = 9_610_004_i64;
    let expired_plex_bid = 9_610_005_i64;

    ctx.db.with_mut_snapshot(|s| {
        s.market_orders.clear();
        s.corporations.entry(corp_id).or_insert_with(|| {
            let mut corp = CorporationRecord::default();
            corp.corporation_id = corp_id;
            corp.ceo_id = char_id;
            corp.name = "ReadPurityCorp".into();
            corp.ticker = "RPUR".into();
            corp.set_division_balance(1, IskCents::from_isk(10_000_000));
            corp
        });
        let rows = [
            (active_trit_ask, char_id, 34_u32, false, 100_i64, 50_i32, 0_i64, now, 90_i64),
            (expired_trit_bid, char_id, 34_u32, true, 90_i64, 40_i32, 360_000_i64, 1_u64, 1_i64),
            (expired_corp_bid, corp_id, 34_u32, true, 80_i64, 30_i32, 240_000_i64, 1_u64, 1_i64),
            (active_plex_ask, char_id, PLEX_TYPE_ID, false, 5_100_000_i64, 5_i32, 0_i64, now, 90_i64),
            (expired_plex_bid, char_id, PLEX_TYPE_ID, true, 4_900_000_i64, 3_i32, 1_470_000_000_i64, 1_u64, 1_i64),
        ];
        for (oid, owner_id, tid, is_bid, price_isk, vol, escrow_cents, issue_unix, duration) in rows {
            s.market_orders.insert(
                oid,
                MarketOrderRecord {
                    order_id: oid,
                    owner_id,
                    station_id: JITA_STATION_ID,
                    region_id: THE_FORGE_REGION_ID,
                    type_id: tid,
                    is_bid,
                    price: IskCents::from_isk(price_isk),
                    vol_entered: vol,
                    vol_remaining: vol,
                    min_volume: 1,
                    is_npc: false,
                    escrow: IskCents(escrow_cents),
                },
            );
            save_market_order_meta(
                s,
                oid,
                MarketOrderMeta {
                    range: -1,
                    duration,
                    issue_unix,
                    order_state: None,
                    account_key: (owner_id == corp_id).then_some(1000),
                    final_vol_remaining: None,
                    closed_unix: None,
                },
            );
        }
    });

    let char_bal_before = ctx.db.get_character(char_id).unwrap().balance;
    let corp_bal_before = ctx.db.with_snapshot(|s| s.corporations[&corp_id].master_wallet_balance());
    let next_eid_before = ctx.db.with_snapshot(|s| s.next_entity_id);
    let _ = ctx.drain_notifications_for_char(char_id);
    sess.notifications.clear();

    // Verify all order-book / personal / corp / system / PLEX getters filter out expired orders on read.
    let get_orders = ctx
        .dispatch_rpc(&mut sess, "marketProxy", None, "GetOrders", &[PyRep::Integer(34)], &kw, None)
        .await
        .expect("GetOrders");
    let PyRep::Tuple(parts) = get_orders else { panic!("Expected Tuple"); };
    assert_eq!(parts[0].crowset_rows().unwrap().len(), 1);
    assert!(parts[1].crowset_rows().unwrap().is_empty());

    let station_asks = ctx
        .dispatch_rpc(&mut sess, "marketProxy", None, "GetStationAsks", &[], &kw, None)
        .await
        .expect("GetStationAsks");
    let station_ask_rows = station_asks.cindexed_rowset_entries().unwrap();
    assert_eq!(station_ask_rows.len(), 2);

    let system_asks = ctx
        .dispatch_rpc(&mut sess, "marketProxy", None, "GetSystemAsks", &[], &kw, None)
        .await
        .expect("GetSystemAsks");
    assert_eq!(system_asks.cindexed_rowset_entries().unwrap().len(), 2);

    let region_best = ctx
        .dispatch_rpc(&mut sess, "marketProxy", None, "GetRegionBest", &[], &kw, None)
        .await
        .expect("GetRegionBest");
    assert_eq!(region_best.cindexed_rowset_entries().unwrap().len(), 2);

    let char_orders = ctx
        .dispatch_rpc(&mut sess, "marketProxy", None, "GetCharOrders", &[], &kw, None)
        .await
        .expect("GetCharOrders");
    let PyRep::Rowset { rows: char_rows, .. } = char_orders else {
        panic!("Expected Rowset from GetCharOrders");
    };
    assert_eq!(char_rows.len(), 2);
    assert!(char_rows.iter().all(|r| {
        let oid = r.first().and_then(PyRep::as_i64);
        oid == Some(active_trit_ask) || oid == Some(active_plex_ask)
    }));

    let char_escrow = ctx
        .dispatch_rpc(&mut sess, "marketProxy", None, "GetCharEscrow", &[], &kw, None)
        .await
        .expect("GetCharEscrow");
    assert_eq!(char_escrow.dict_get("escrow").and_then(PyRep::as_f64), Some(0.0));
    assert_eq!(char_escrow.dict_get("totalRemainingCostISK").and_then(PyRep::as_f64), Some(0.0));
    assert_eq!(char_escrow.dict_get("itemsEscrow").and_then(PyRep::as_f64), Some(25_505_000.0));

    let corp_orders = ctx
        .dispatch_rpc(&mut sess, "marketProxy", None, "GetCorporationOrders", &[], &kw, None)
        .await
        .expect("GetCorporationOrders");
    let PyRep::Rowset { rows: corp_rows, .. } = corp_orders else {
        panic!("Expected Rowset from GetCorporationOrders");
    };
    assert!(corp_rows.is_empty());

    let plex_best = ctx
        .dispatch_rpc(&mut sess, "marketProxy", None, "GetPlexBest", &[], &kw, None)
        .await
        .expect("GetPlexBest");
    assert!(plex_best.is_cindexed_rowset());
    let pb_entries = plex_best.cindexed_rowset_entries().unwrap();
    let pb_row = pb_entries
        .iter()
        .find(|(k, _)| k == &PyRep::Integer(PLEX_TYPE_ID as i64))
        .map(|(_, v)| v)
        .expect("GetPlexBest must key active PLEX ask by integer typeID 44992");
    assert_eq!(pb_row.dbrow_get("typeID"), Some(&PyRep::Integer(PLEX_TYPE_ID as i64)));
    assert_eq!(pb_row.dbrow_get("price").and_then(PyRep::as_f64), Some(5_100_000.0));
    assert_eq!(pb_row.dbrow_get("volRemaining").and_then(PyRep::as_f64), Some(5.0));
    assert_eq!(pb_row.dbrow_get("stationID"), Some(&PyRep::Integer(JITA_STATION_ID)));

    let plex_orders = ctx
        .dispatch_rpc(&mut sess, "marketProxy", None, "GetPlexOrders", &[], &kw, None)
        .await
        .expect("GetPlexOrders");
    let PyRep::Tuple(plex_parts) = plex_orders else { panic!("Expected Tuple"); };
    assert_eq!(plex_parts[0].crowset_rows().unwrap().len(), 1);
    assert!(plex_parts[1].crowset_rows().unwrap().is_empty());

    // Call history getters through full dispatch_rpc too.
    for (method, args) in [
        ("GetHistoryForManyTypeIDs", vec![PyRep::List(vec![PyRep::Integer(PLEX_TYPE_ID as i64), PyRep::Integer(34)])]),
        ("GetOldPriceHistory", vec![PyRep::Integer(PLEX_TYPE_ID as i64)]),
        ("GetNewPriceHistory", vec![PyRep::Integer(34)]),
        ("GetPlexHistory", vec![]),
        ("GetPlexNewPriceHistory", vec![]),
        ("GetPlexOldPriceHistory", vec![]),
    ] {
        ctx.dispatch_rpc(&mut sess, "marketProxy", None, method, &args, &kw, None)
            .await
            .unwrap_or_else(|e| panic!("{method} dispatch_rpc failed: {e:?}"));
    }

    // GetMarketOrderHistory must stay strictly read-only even when unswept expired orders exist,
    // projecting expired orders as orderState=4 with their remaining volume.
    let order_hist_pre = ctx
        .dispatch_rpc(&mut sess, "marketProxy", None, "GetMarketOrderHistory", &[PyRep::Bool(false)], &kw, None)
        .await
        .expect("GetMarketOrderHistory(false)");
    let PyRep::Rowset { rows: hist_rows_pre, .. } = order_hist_pre else {
        panic!("Expected Rowset from GetMarketOrderHistory");
    };
    assert_eq!(hist_rows_pre.len(), 2);
    for (oid, expected_vol) in [(expired_trit_bid, 40_i64), (expired_plex_bid, 3_i64)] {
        let r = hist_rows_pre
            .iter()
            .find(|r| r.first().and_then(PyRep::as_i64) == Some(oid))
            .expect("unswept expired personal order must appear in history");
        assert_eq!(r[9].as_i64(), Some(expected_vol));
        assert_eq!(r[11].as_i64(), Some(4));
    }

    let order_hist_corp_pre = ctx
        .dispatch_rpc(&mut sess, "marketProxy", None, "GetMarketOrderHistory", &[PyRep::Bool(true)], &kw, None)
        .await
        .expect("GetMarketOrderHistory(true)");
    let PyRep::Rowset { rows: hist_corp_rows_pre, .. } = order_hist_corp_pre else {
        panic!("Expected Rowset from GetMarketOrderHistory(true)");
    };
    assert_eq!(hist_corp_rows_pre.len(), 3);
    assert!(hist_corp_rows_pre.iter().any(|r| {
        r.first().and_then(PyRep::as_i64) == Some(expired_corp_bid)
            && r[9].as_i64() == Some(30)
            && r[11].as_i64() == Some(4)
    }));

    // Confirm ZERO world-state mutation occurred across any read getter!
    ctx.db.with_snapshot(|s| {
        assert_eq!(s.market_orders.len(), 5);
        for (oid, expected_vol) in [
            (active_trit_ask, 50),
            (expired_trit_bid, 40),
            (expired_corp_bid, 30),
            (active_plex_ask, 5),
            (expired_plex_bid, 3),
        ] {
            assert_eq!(s.market_orders[&oid].vol_remaining, expected_vol);
        }
        assert_eq!(s.next_entity_id, next_eid_before);
        assert_eq!(s.corporations[&corp_id].master_wallet_balance(), corp_bal_before);
    });
    assert_eq!(ctx.db.get_character(char_id).unwrap().balance, char_bal_before);
    assert!(ctx.drain_notifications_for_char(char_id).is_empty());
    assert!(sess.notifications.is_empty());

    // After sweeping expired orders, GetMarketOrderHistory with zero expired orders stays snapshot-read-pure
    // and continues returning the swept orders.
    assert_eq!(
        crate::services::market_contracts::sweep_expired_orders_and_notify(&ctx, ctx.now_unix()),
        3
    );
    let _ = ctx.drain_notifications_for_char(char_id);
    sess.notifications.clear();
    let eid_clean = ctx.db.with_snapshot(|s| s.next_entity_id);
    let order_hist = ctx
        .dispatch_rpc(&mut sess, "marketProxy", None, "GetMarketOrderHistory", &[PyRep::Bool(false)], &kw, None)
        .await
        .expect("GetMarketOrderHistory");
    let PyRep::Rowset { rows: hist_rows_post, .. } = order_hist else {
        panic!("Expected Rowset from GetMarketOrderHistory");
    };
    assert_eq!(hist_rows_post.len(), 2);
    assert_eq!(ctx.db.with_snapshot(|s| s.next_entity_id), eid_clean);
    assert!(ctx.drain_notifications_for_char(char_id).is_empty());
}

#[test]
fn plex_best_orders_filter_offline_and_acl_denied_upwell_structures_and_history_rejects_u32_truncation() {
    use crate::services::market_trading::{save_market_order_meta, MarketOrderMeta};
    use crate::services::station_structure::structure_upwell_ops::{
        load_ext_state, save_ext_state, AccessGroupRecord, StructureAccessProfile,
    };
    use crate::services::structure_profile_services::SETTING_MARKET_TAX;
    use eve_db::{
        CorporationRecord, InventoryItemRecord, MarketHistoryDayRecord, UpwellStructureRecord,
    };

    let ctx = test_ctx();
    let caller_id = 90_000_001_i64;
    let caller_corp_id = ctx.db.with_snapshot(|s| s.characters[&caller_id].corporation_id);
    let mut sess = jita_session(caller_id, caller_corp_id);
    let kw = HashMap::new();
    let now = ctx.now_unix();

    let struct_owner_corp = 98_910_001_i64;
    let offline_struct_id = 10_000_910_101_i64;
    let acl_denied_struct_id = 10_000_910_102_i64;
    let offline_ask_oid = 9_620_001_i64;
    let acl_denied_ask_oid = 9_620_002_i64;
    let accessible_ask_oid = 9_620_003_i64;
    let acl_denied_bid_oid = 9_620_004_i64;
    let accessible_bid_oid = 9_620_005_i64;

    ctx.db.with_mut_snapshot(|s| {
        s.market_orders.clear();
        s.corporations.insert(
            struct_owner_corp,
            CorporationRecord::new(struct_owner_corp, "StructOwner", "STOW", 90_000_002),
        );
        for (sid, online) in [(offline_struct_id, false), (acl_denied_struct_id, true)] {
            s.structures.insert(
                sid,
                UpwellStructureRecord {
                    structure_id: sid,
                    type_id: 35_833,
                    owner_corp_id: struct_owner_corp,
                    solar_system_id: 30_000_142,
                    name: format!("PlexStruct-{sid}"),
                    state: "ShieldVulnerable".into(),
                    quantum_core_installed: true,
                    fuel_blocks: if online { 500 } else { 0 },
                    ..Default::default()
                },
            );
            let mod_id = sid + 1;
            s.items.insert(
                mod_id,
                InventoryItemRecord {
                    item_id: mod_id,
                    type_id: 35_892,
                    owner_id: struct_owner_corp,
                    location_id: sid,
                    flag: InventoryFlag::from_u16(164),
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
            s.module_online_states.insert(mod_id, online);
        }

        let mut ext = load_ext_state(s);
        ext.owner_groups.insert(
            9101,
            AccessGroupRecord {
                group_id: 9101,
                creator_id: 90_000_002,
                owner_corp_id: struct_owner_corp,
                name: "AllowedOnly90000002".into(),
                description: String::new(),
                members: HashMap::from([(90_000_002, 0)]),
            },
        );
        let mut acl_prof = StructureAccessProfile {
            profile_id: 9101,
            corp_only: false,
            ..Default::default()
        };
        acl_prof
            .service_acl_rules
            .insert(SETTING_MARKET_TAX, vec![(9101, 100)]);
        ext.access_profiles.insert(acl_denied_struct_id, acl_prof);
        save_ext_state(s, &ext);

        let rows = [
            (offline_ask_oid, offline_struct_id, false, 4_500_000_i64),
            (acl_denied_ask_oid, acl_denied_struct_id, false, 4_200_000_i64),
            (accessible_ask_oid, JITA_STATION_ID, false, 5_000_000_i64),
            (acl_denied_bid_oid, acl_denied_struct_id, true, 4_900_000_i64),
            (accessible_bid_oid, JITA_STATION_ID, true, 4_700_000_i64),
        ];
        for (oid, st_id, is_bid, price_isk) in rows {
            s.market_orders.insert(
                oid,
                MarketOrderRecord {
                    order_id: oid,
                    owner_id: 90_000_002,
                    station_id: st_id,
                    region_id: THE_FORGE_REGION_ID,
                    type_id: PLEX_TYPE_ID,
                    is_bid,
                    price: IskCents::from_isk(price_isk),
                    vol_entered: 10,
                    vol_remaining: 10,
                    min_volume: 1,
                    is_npc: false,
                    escrow: if is_bid { IskCents::from_isk(price_isk * 10) } else { IskCents(0) },
                },
            );
            save_market_order_meta(
                s,
                oid,
                MarketOrderMeta {
                    range: -1,
                    duration: 90,
                    issue_unix: now,
                    order_state: None,
                    account_key: None,
                    final_vol_remaining: None,
                    closed_unix: None,
                },
            );
        }

        s.market_history.insert(
            (THE_FORGE_REGION_ID, PLEX_TYPE_ID),
            vec![MarketHistoryDayRecord {
                date: "2026-11-19".into(),
                lowest: 4_900_000.0,
                highest: 5_100_000.0,
                average: 5_000_000.0,
                volume: 5,
                order_count: 1,
                open_price: None,
                close_price: None,
            }],
        );
    });

    // GetPlexBest must omit offline and ACL-denied structures, returning the 5.0M Jita ask.
    let best = handle(&ctx, &mut sess, "marketProxy", "GetPlexBest", &[], &kw)
        .unwrap()
        .expect("GetPlexBest");
    let entries = best.cindexed_rowset_entries().unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].1.dbrow_get("price").and_then(PyRep::as_f64), Some(5_000_000.0));
    assert_eq!(
        entries[0].1.dbrow_get("stationID"),
        Some(&PyRep::Integer(JITA_STATION_ID))
    );

    // GetPlexOrders must omit both sells and buys inside offline / ACL-denied structures.
    let plex_orders = handle(&ctx, &mut sess, "marketProxy", "GetPlexOrders", &[], &kw)
        .unwrap()
        .expect("GetPlexOrders");
    let PyRep::Tuple(parts) = plex_orders else {
        panic!("Expected Tuple");
    };
    let sells = parts[0].crowset_rows().unwrap();
    let buys = parts[1].crowset_rows().unwrap();
    assert_eq!(sells.len(), 1);
    assert_eq!(
        sells[0].dbrow_get("orderID").and_then(PyRep::as_i64),
        Some(accessible_ask_oid)
    );
    assert_eq!(buys.len(), 1);
    assert_eq!(
        buys[0].dbrow_get("orderID").and_then(PyRep::as_i64),
        Some(accessible_bid_oid)
    );

    // GetHistoryForManyTypeIDs must not truncate 64-bit IDs modulo 2^32 to match PLEX_TYPE_ID.
    let truncated_collision = (1_i64 << 32) | (PLEX_TYPE_ID as i64);
    let hist = handle(
        &ctx,
        &mut sess,
        "marketProxy",
        "GetHistoryForManyTypeIDs",
        &[PyRep::List(vec![
            PyRep::Integer(PLEX_TYPE_ID as i64),
            PyRep::Integer(truncated_collision),
        ])],
        &kw,
    )
    .unwrap()
    .expect("GetHistoryForManyTypeIDs");
    let PyRep::Dict(dict_entries) = hist else {
        panic!("Expected Dict");
    };
    let plex_hist_rows = dict_entries
        .iter()
        .find(|(k, _)| k == &PyRep::Integer(PLEX_TYPE_ID as i64))
        .and_then(|(_, v)| v.crowset_rows())
        .unwrap();
    assert_eq!(plex_hist_rows.len(), 1);
    let bogus_hist_rows = dict_entries
        .iter()
        .find(|(k, _)| k == &PyRep::Integer(truncated_collision))
        .and_then(|(_, v)| v.crowset_rows())
        .unwrap();
    assert!(bogus_hist_rows.is_empty());
}

#[test]
fn rejected_modify_and_plex_sell_calls_do_not_sweep_expired_orders() {
    use crate::services::market_trading::{save_market_order_meta, MarketOrderMeta};
    use crate::services::station_structure::structure_upwell_ops::{
        load_ext_state, save_ext_state, AccessGroupRecord, StructureAccessProfile,
    };
    use crate::services::structure_profile_services::SETTING_MARKET_TAX;
    use eve_db::{CorporationRecord, InventoryItemRecord, UpwellStructureRecord};

    let ctx = test_ctx();
    let owner_id = 90_000_001_i64;
    let attacker_id = 90_000_002_i64;
    let now = ctx.now_unix();
    let kw = HashMap::new();

    let (owner_corp, attacker_corp) = ctx.db.with_snapshot(|s| {
        (
            s.characters[&owner_id].corporation_id,
            s.characters[&attacker_id].corporation_id,
        )
    });
    let active_trit_oid = 9_630_001_i64;
    let cooldown_oid = 9_630_002_i64;
    let expired_oid = 9_630_003_i64;
    let acl_struct_id = 10_000_910_201_i64;
    let struct_owner_corp = 98_910_201_i64;

    ctx.db.with_mut_snapshot(|s| {
        s.market_orders.retain(|_, o| o.is_npc);
        let cur_vault = s.plex_vault_balance(owner_id);
        if cur_vault > 0 {
            let _ = s.adjust_plex_vault(owner_id, -cur_vault);
        }
        if let Some(ch) = s.characters.get_mut(&owner_id) {
            ch.plex_balance = 0;
        }
        s.items.retain(|_, i| !(i.owner_id == owner_id && i.type_id == PLEX_TYPE_ID));
        s.corporations.insert(
            struct_owner_corp,
            CorporationRecord::new(struct_owner_corp, "PlexGateCorp", "PXGC", attacker_id),
        );
        s.structures.insert(
            acl_struct_id,
            UpwellStructureRecord {
                structure_id: acl_struct_id,
                type_id: 35_833,
                owner_corp_id: struct_owner_corp,
                solar_system_id: 30_000_142,
                name: "PlexGateFort".into(),
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 500,
                ..Default::default()
            },
        );
        s.items.insert(
            acl_struct_id + 1,
            InventoryItemRecord {
                item_id: acl_struct_id + 1,
                type_id: 35_892,
                owner_id: struct_owner_corp,
                location_id: acl_struct_id,
                flag: InventoryFlag::from_u16(164),
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        s.module_online_states.insert(acl_struct_id + 1, true);
        let mut ext = load_ext_state(s);
        ext.owner_groups.insert(
            9102,
            AccessGroupRecord {
                group_id: 9102,
                creator_id: attacker_id,
                owner_corp_id: struct_owner_corp,
                name: "OnlyAttacker".into(),
                description: String::new(),
                members: HashMap::from([(attacker_id, 0)]),
            },
        );
        let mut prof = StructureAccessProfile {
            profile_id: 9102,
            corp_only: false,
            ..Default::default()
        };
        prof.service_acl_rules.insert(SETTING_MARKET_TAX, vec![(9102, 100)]);
        ext.access_profiles.insert(acl_struct_id, prof);
        save_ext_state(s, &ext);

        for (oid, oid_owner, tid, issue_unix, duration, escrow_isk) in [
            (active_trit_oid, owner_id, 34_u32, now.saturating_sub(600), 90_i64, 100_i64),
            (cooldown_oid, owner_id, 34_u32, now, 90_i64, 100_i64),
            (expired_oid, attacker_id, 34_u32, now.saturating_sub(2 * 86_400), 1_i64, 50_i64),
        ] {
            s.market_orders.insert(
                oid,
                MarketOrderRecord {
                    order_id: oid,
                    owner_id: oid_owner,
                    station_id: JITA_STATION_ID,
                    region_id: THE_FORGE_REGION_ID,
                    type_id: tid,
                    is_bid: true,
                    price: IskCents::from_isk(10),
                    vol_entered: 5,
                    vol_remaining: 5,
                    min_volume: 1,
                    is_npc: false,
                    escrow: IskCents::from_isk(escrow_isk),
                },
            );
            save_market_order_meta(
                s,
                oid,
                MarketOrderMeta {
                    range: -1,
                    duration,
                    issue_unix,
                    order_state: None,
                    account_key: None,
                    final_vol_remaining: None,
                    closed_unix: None,
                },
            );
        }
    });

    let _ = ctx.drain_notifications_for_char(attacker_id);
    let eid_before = ctx.db.with_snapshot(|s| s.next_entity_id);
    let mut owner_sess = jita_session(owner_id, owner_corp);
    let mut attacker_sess = jita_session(attacker_id, attacker_corp);

    assert_eq!(
        handle(
            &ctx,
            &mut owner_sess,
            "marketProxy",
            "ModifyCharOrder",
            &[PyRep::Integer(99_999_999), PyRep::Real(12.0)],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "OrderNotFound"
    );
    assert_eq!(
        handle(
            &ctx,
            &mut attacker_sess,
            "marketProxy",
            "ModifyCharOrder",
            &[PyRep::Integer(active_trit_oid), PyRep::Real(12.0)],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "OrderOwnershipViolation"
    );
    assert_eq!(
        handle(
            &ctx,
            &mut owner_sess,
            "marketProxy",
            "ModifyPlexCharOrder",
            &[PyRep::Integer(active_trit_oid), PyRep::Real(12.0)],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidPlexOrder"
    );
    assert_eq!(
        handle(
            &ctx,
            &mut owner_sess,
            "marketProxy",
            "ModifyCharOrder",
            &[PyRep::Integer(cooldown_oid), PyRep::Real(12.0)],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "OrderModificationCooldown"
    );
    assert_eq!(
        handle(
            &ctx,
            &mut owner_sess,
            "marketProxy",
            "PlacePlexSellOrder",
            &[PyRep::Real(5_000_000.0), PyRep::Integer(1), PyRep::Integer(90)],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InsufficientPlexBalance"
    );
    owner_sess.station_id = Some(acl_struct_id);
    assert_eq!(
        handle(
            &ctx,
            &mut owner_sess,
            "marketProxy",
            "PlacePlexSellOrder",
            &[PyRep::Real(5_000_000.0), PyRep::Integer(1), PyRep::Integer(90)],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "StructureServiceAccessDenied"
    );

    ctx.db.with_snapshot(|s| {
        assert_eq!(s.market_orders[&expired_oid].vol_remaining, 5);
        assert_eq!(s.next_entity_id, eid_before);
    });
    assert!(ctx.drain_notifications_for_char(attacker_id).is_empty());
}

#[cfg(test)]
#[path = "market_contracts_extra_unit_tests.rs"]
mod market_contracts_extra_unit_tests;
