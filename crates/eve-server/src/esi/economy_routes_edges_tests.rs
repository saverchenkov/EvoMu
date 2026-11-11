//! Edge-case and full-branch unit tests for `esi::economy_routes`.

#![forbid(unsafe_code)]

use super::*;
use crate::esi::full_endpoint_catalog_tests::seed_full_esi_context;
use eve_db::snapshot_ext::{MarketHistoryDayRecord, MarketOrderMeta};
use eve_db::{
    ContractRecord, IndustryJobRecord, InventoryFlag, InventoryItemRecord, IskCents,
    MarketOrderRecord, SsoTokenClaims, UpwellStructureRecord,
};

fn params(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
        .collect()
}

fn body(res: &RouteExecutionResult) -> &Value {
    res.body.as_ref().expect("response body")
}

fn make_claims(char_id: i64) -> SsoTokenClaims {
    SsoTokenClaims {
        sub: format!("CHARACTER:EVE:{char_id}"),
        name: "Test Pilot".to_string(),
        owner: "owner-hash".to_string(),
        user_id: 1,
        character_id: char_id,
        scp: vec![
            "esi-markets.structure_markets.v1".to_string(),
            "esi-markets.read_character_orders.v1".to_string(),
            "esi-markets.read_corporation_orders.v1".to_string(),
            "esi-contracts.read_character_contracts.v1".to_string(),
            "esi-contracts.read_corporation_contracts.v1".to_string(),
        ],
        iss: "http://localhost".to_string(),
        aud: vec!["EVE Online".to_string()],
        iat: 1_700_000_000,
        exp: 1_800_000_000,
        jti: "jti-1".to_string(),
    }
}

#[test]
fn contract_helpers_cover_retention_party_corp_region_and_items() {
    let (ctx, _token, char_id, corp_id, _planet_id) = seed_full_esi_context();
    let now = ctx.now_unix();

    // contract_retained_in_list branches when date_issued_unix == 0
    let mut c_no_issued = ContractRecord {
        contract_id: 90_001,
        issuer_id: char_id,
        issuer_corp_id: 0,
        assignee_id: 0,
        acceptor_id: None,
        contract_type: "ItemExchange".to_string(),
        availability: String::new(),
        status: "Outstanding".to_string(),
        title: "No issued".to_string(),
        for_corp: false,
        price: IskCents::from_isk(100),
        reward: IskCents::ZERO,
        collateral: IskCents::ZERO,
        buyout: IskCents::ZERO,
        start_station_id: 1_055_000_001,
        end_station_id: 1_055_000_001,
        date_issued_unix: 0,
        num_days: 7,
        expiration_unix: now + 86_400,
        item_ids: vec![999_999_999, 890_001],
        requested_items: vec![(34, 10)],
        current_bid: IskCents::ZERO,
        current_bidder_id: None,
        bid_history: vec![],
    };
    assert!(contract_retained_in_list(&c_no_issued, now));
    c_no_issued.expiration_unix = now.saturating_sub(CONTRACT_RETENTION_SECS + 100);
    assert!(!contract_retained_in_list(&c_no_issued, now));
    c_no_issued.expiration_unix = 0;
    assert!(contract_retained_in_list(&c_no_issued, now));

    assert!(is_public_available_contract(&c_no_issued));
    c_no_issued.assignee_id = char_id;
    assert!(!is_public_available_contract(&c_no_issued));

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            890_001,
            InventoryItemRecord {
                item_id: 890_001,
                type_id: 34,
                owner_id: char_id,
                location_id: 60_003_760,
                flag: InventoryFlag::Hangar,
                quantity: 25,
                singleton: false,
                ..Default::default()
            },
        );
    });

    ctx.db.with_snapshot(|snap| {
        // Upwell structure start_station_id in region 10000002 vs unknown location
        assert!(contract_in_region(&ctx, snap, &c_no_issued, 10_000_002));
        assert!(!contract_in_region(&ctx, snap, &c_no_issued, 10_000_043));
        let mut c_unknown_loc = c_no_issued.clone();
        c_unknown_loc.start_station_id = 777_777_777;
        assert!(!contract_in_region(&ctx, snap, &c_unknown_loc, 10_000_002));

        // Party checks: corp assignee, corp availability, alliance assignee
        let mut c_corp_assign = c_no_issued.clone();
        c_corp_assign.issuer_id = 90_000_099;
        c_corp_assign.assignee_id = corp_id;
        assert!(is_character_party_to_contract(snap, char_id, &c_corp_assign));

        let mut c_corp_avail = c_no_issued.clone();
        c_corp_avail.issuer_id = 90_000_099;
        c_corp_avail.assignee_id = 0;
        c_corp_avail.availability = "Corporation".to_string();
        c_corp_avail.issuer_corp_id = corp_id;
        assert!(is_character_party_to_contract(snap, char_id, &c_corp_avail));

        let mut c_ally = c_no_issued.clone();
        c_ally.issuer_id = 90_000_099;
        c_ally.assignee_id = 99_000_001;
        assert!(is_character_party_to_contract(snap, char_id, &c_ally));
        assert!(!is_character_party_to_contract(snap, 999_999_999, &c_ally));

        // is_corp_contract branches
        let corp = snap.corporations.get(&corp_id).unwrap();
        let mut c_corp_fallback = c_no_issued.clone();
        c_corp_fallback.for_corp = true;
        c_corp_fallback.issuer_corp_id = 0;
        assert!(is_corp_contract(corp, corp_id, &c_corp_fallback));
        let mut c_corp_acceptor = c_no_issued.clone();
        c_corp_acceptor.issuer_id = 90_000_099;
        c_corp_acceptor.assignee_id = 0;
        c_corp_acceptor.acceptor_id = Some(corp_id);
        assert!(is_corp_contract(corp, corp_id, &c_corp_acceptor));

        // contract_item_rows with public=true vs false, missing item skipped
        let pub_items = contract_item_rows(snap, &c_no_issued, true);
        assert_eq!(pub_items.len(), 2);
        assert_eq!(pub_items[0]["item_id"], 890_001);
        assert!(pub_items[0].get("is_singleton").is_none());
        assert_eq!(pub_items[1]["is_included"], false);
        assert!(pub_items[1].get("is_singleton").is_none());

        let priv_items = contract_item_rows(snap, &c_no_issued, false);
        assert_eq!(priv_items[0]["is_singleton"], false);
        assert_eq!(priv_items[1]["is_singleton"], false);
    });
}

#[test]
fn market_groups_prices_region_orders_history_and_types_edge_paths() {
    let (ctx, _token, char_id, _corp_id, _planet_id) = seed_full_esi_context();
    let empty_q = HashMap::new();
    let p_bad_reg = params(&[("region_id", "bad")]);
    let p_big_reg = params(&[("region_id", "999999999999")]);
    let p_miss_reg = params(&[("region_id", "19999999")]);
    let p_jita = params(&[("region_id", "10000002")]);

    // /markets/groups/{market_group_id}: 400, u32 overflow 404, non-en language, parent_group_id Some & None
    assert_eq!(
        dispatch_economy_route(
            &ctx,
            "GET",
            "/markets/groups/{market_group_id}",
            &params(&[("market_group_id", "bad")]),
            &empty_q,
            None,
            1
        )
        .unwrap()
        .status_code,
        400
    );
    assert_eq!(
        dispatch_economy_route(
            &ctx,
            "GET",
            "/markets/groups/{market_group_id}",
            &params(&[("market_group_id", "999999999999")]),
            &empty_q,
            None,
            1
        )
        .unwrap()
        .status_code,
        404
    );
    let root_mg = ctx
        .sde
        .market_groups
        .values()
        .find(|m| m.parent_group_id.is_none())
        .map(|m| m.market_group_id)
        .unwrap();
    let child_mg = ctx
        .sde
        .market_groups
        .values()
        .find(|m| m.parent_group_id.is_some())
        .map(|m| m.market_group_id)
        .unwrap();
    let res_root = dispatch_economy_route(
        &ctx,
        "GET",
        "/markets/groups/{market_group_id}",
        &params(&[("market_group_id", &root_mg.to_string())]),
        &params(&[("language", "de")]),
        None,
        1,
    )
    .unwrap();
    assert_eq!(res_root.status_code, 200);
    assert!(body(&res_root).get("parent_group_id").is_none());

    let res_child = dispatch_economy_route(
        &ctx,
        "GET",
        "/markets/groups/{market_group_id}",
        &params(&[("market_group_id", &child_mg.to_string())]),
        &params(&[("language", "fr")]),
        None,
        1,
    )
    .unwrap();
    assert_eq!(res_child.status_code, 200);
    assert!(body(&res_child).get("parent_group_id").is_some());

    // Seed /markets/prices edge cases:
    // - history with 0 volume -> simple average fallback
    // - live_adjusted only (no history, no orders) + live_adjusted with buy order only
    // - order only (no history, no live_adjusted)
    // - order missing meta and Upwell order skipped in /markets/{region_id}/orders
    let now = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        snap.market_history.insert(
            (10_000_002, 35),
            vec![
                MarketHistoryDayRecord {
                    date: "2026-01-01".into(),
                    average: 10.0,
                    highest: 12.0,
                    lowest: 8.0,
                    order_count: 1,
                    volume: 0,
                    open_price: None,
                    close_price: None,
                },
                MarketHistoryDayRecord {
                    date: "2026-01-02".into(),
                    average: 20.0,
                    highest: 22.0,
                    lowest: 18.0,
                    order_count: 1,
                    volume: 0,
                    open_price: None,
                    close_price: None,
                },
            ],
        );
        // Active industry job producing type 36 -> live_adjusted only (no history, no orders)
        // and type 37 -> live_adjusted + buy order
        snap.industry_jobs.insert(
            990_001,
            IndustryJobRecord {
                job_id: 990_001,
                installer_id: char_id,
                facility_id: 60_003_760,
                blueprint_item_id: 990_002,
                blueprint_type_id: 688,
                activity_id: 1,
                runs: 1,
                product_type_id: 36,
                status: "Active".to_string(),
                duration_seconds: 60,
                start_unix: now,
                blueprint_location_id: 60_003_760,
                output_location_id: 60_003_760,
                cost: Some(1000.0),
                ..Default::default()
            },
        );
        snap.market_orders.insert(
            991_001,
            MarketOrderRecord {
                order_id: 991_001,
                type_id: 38,
                owner_id: char_id,
                station_id: 60_003_760,
                region_id: 10_000_002,
                price: IskCents::from_isk(50),
                vol_entered: 10,
                vol_remaining: 10,
                min_volume: 1,
                is_bid: true,
                escrow: IskCents::from_isk(500),
                is_npc: false,
            },
        );
        // Order missing market_order_meta -> skipped in regional orders
        snap.market_orders.insert(
            991_002,
            MarketOrderRecord {
                order_id: 991_002,
                type_id: 39,
                owner_id: char_id,
                station_id: 1_055_000_001,
                region_id: 10_000_002,
                price: IskCents::from_isk(60),
                vol_entered: 10,
                vol_remaining: 10,
                min_volume: 1,
                is_bid: false,
                escrow: IskCents::ZERO,
                is_npc: false,
            },
        );
        snap.market_order_meta.insert(
            991_002,
            MarketOrderMeta {
                issued_unix: now,
                duration_days: 90,
                range: "station".into(),
                issued_by: Some(char_id),
                wallet_division: Some(1),
                is_corporation: false,
                state: None,
            },
        );
    });
    let prices = dispatch_economy_route(&ctx, "GET", "/markets/prices", &empty_q, &empty_q, None, 1).unwrap();
    assert_eq!(prices.status_code, 200);
    let p_arr = body(&prices).as_array().unwrap();
    let p35 = p_arr.iter().find(|r| r["type_id"] == 35).unwrap();
    assert_eq!(p35["average_price"], 15.0);

    // /markets/{region_id}/orders errors & pagination
    for p in [&p_bad_reg, &p_big_reg, &p_miss_reg] {
        let code = dispatch_economy_route(&ctx, "GET", "/markets/{region_id}/orders", p, &empty_q, None, 1)
            .unwrap()
            .status_code;
        assert!(code == 400 || code == 404);
    }
    assert_eq!(
        dispatch_economy_route(
            &ctx,
            "GET",
            "/markets/{region_id}/orders",
            &p_jita,
            &params(&[("type_id", "0")]),
            None,
            1
        )
        .unwrap()
        .status_code,
        400
    );
    assert_eq!(
        dispatch_economy_route(&ctx, "GET", "/markets/{region_id}/orders", &p_jita, &empty_q, None, 999)
            .unwrap()
            .status_code,
        404
    );

    // /markets/{region_id}/history errors
    for p in [&p_bad_reg, &p_big_reg, &p_miss_reg] {
        let code = dispatch_economy_route(
            &ctx,
            "GET",
            "/markets/{region_id}/history",
            p,
            &params(&[("type_id", "34")]),
            None,
            1,
        )
        .unwrap()
        .status_code;
        assert!(code == 400 || code == 404);
    }
    assert_eq!(
        dispatch_economy_route(
            &ctx,
            "GET",
            "/markets/{region_id}/history",
            &p_jita,
            &params(&[("type_id", "abc")]),
            None,
            1
        )
        .unwrap()
        .status_code,
        400
    );
    assert_eq!(
        dispatch_economy_route(
            &ctx,
            "GET",
            "/markets/{region_id}/history",
            &p_jita,
            &params(&[("type_id", "99999999")]),
            None,
            1
        )
        .unwrap()
        .status_code,
        404
    );

    // /markets/{region_id}/types errors & pagination
    for p in [&p_bad_reg, &p_big_reg, &p_miss_reg] {
        let code = dispatch_economy_route(&ctx, "GET", "/markets/{region_id}/types", p, &empty_q, None, 1)
            .unwrap()
            .status_code;
        assert!(code == 400 || code == 404);
    }
    assert_eq!(
        dispatch_economy_route(&ctx, "GET", "/markets/{region_id}/types", &p_jita, &empty_q, None, 999)
            .unwrap()
            .status_code,
        404
    );
}

#[test]
fn structure_char_corp_market_orders_and_contract_routes_cover_all_error_and_acl_branches() {
    let (ctx, _token, char_id, corp_id, _planet_id) = seed_full_esi_context();
    let empty_q = HashMap::new();
    let now = ctx.now_unix();
    let claims = make_claims(char_id);
    let fortizar_id = 1_055_000_888_i64;

    // Seed Fortizar with online Standup Market Hub (35892), orders (char, corp, structure), and contracts
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            fortizar_id,
            UpwellStructureRecord {
                structure_id: fortizar_id,
                type_id: 35_833, // Fortizar (Large -> allows market)
                owner_corp_id: corp_id,
                solar_system_id: 30_000_142,
                name: "Market Fortizar".into(),
                position: [1000.0, 2000.0, 3000.0],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 500,
                shield_hp: 100_000.0,
                armor_hp: 100_000.0,
                hull_hp: 100_000.0,
                ..Default::default()
            },
        );
        snap.items.insert(
            992_001,
            InventoryItemRecord {
                item_id: 992_001,
                type_id: 35_892,
                owner_id: corp_id,
                location_id: fortizar_id,
                flag: InventoryFlag::ServiceSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.module_online_states.insert(992_001, true);

        // Structure order (valid meta) + structure order missing meta
        snap.market_orders.insert(
            992_010,
            MarketOrderRecord {
                order_id: 992_010,
                type_id: 34,
                owner_id: char_id,
                station_id: fortizar_id,
                region_id: 10_000_002,
                price: IskCents::from_isk(12),
                vol_entered: 100,
                vol_remaining: 50,
                min_volume: 1,
                is_bid: false,
                escrow: IskCents::ZERO,
                is_npc: false,
            },
        );
        snap.market_order_meta.insert(
            992_010,
            MarketOrderMeta {
                issued_unix: now,
                duration_days: 30,
                range: "station".into(),
                issued_by: Some(char_id),
                wallet_division: Some(1),
                is_corporation: false,
                state: None,
            },
        );
        snap.market_orders.insert(
            992_011,
            MarketOrderRecord {
                order_id: 992_011,
                type_id: 34,
                owner_id: char_id,
                station_id: fortizar_id,
                region_id: 10_000_002,
                price: IskCents::from_isk(13),
                vol_entered: 10,
                vol_remaining: 10,
                min_volume: 1,
                is_bid: false,
                escrow: IskCents::ZERO,
                is_npc: false,
            },
        );

        // Char history order missing state (skipped)
        snap.market_orders.insert(
            992_012,
            MarketOrderRecord {
                order_id: 992_012,
                type_id: 34,
                owner_id: char_id,
                station_id: 60_003_760,
                region_id: 10_000_002,
                price: IskCents::from_isk(10),
                vol_entered: 10,
                vol_remaining: 0,
                min_volume: 1,
                is_bid: false,
                escrow: IskCents::ZERO,
                is_npc: false,
            },
        );
        snap.market_order_meta.insert(
            992_012,
            MarketOrderMeta {
                issued_unix: now,
                duration_days: 30,
                range: "station".into(),
                issued_by: None,
                wallet_division: None,
                is_corporation: false,
                state: None,
            },
        );

        // Corp active orders: one missing issued_by, one !is_corporation, one valid
        snap.market_orders.insert(
            992_013,
            MarketOrderRecord {
                order_id: 992_013,
                type_id: 34,
                owner_id: corp_id,
                station_id: 60_003_760,
                region_id: 10_000_002,
                price: IskCents::from_isk(10),
                vol_entered: 10,
                vol_remaining: 5,
                min_volume: 1,
                is_bid: false,
                escrow: IskCents::ZERO,
                is_npc: false,
            },
        );
        snap.market_order_meta.insert(
            992_013,
            MarketOrderMeta {
                issued_unix: now,
                duration_days: 30,
                range: "station".into(),
                issued_by: None,
                wallet_division: Some(1),
                is_corporation: true,
                state: None,
            },
        );
        snap.market_orders.insert(
            992_014,
            MarketOrderRecord {
                order_id: 992_014,
                type_id: 34,
                owner_id: corp_id,
                station_id: 60_003_760,
                region_id: 10_000_002,
                price: IskCents::from_isk(11),
                vol_entered: 10,
                vol_remaining: 0,
                min_volume: 1,
                is_bid: false,
                escrow: IskCents::ZERO,
                is_npc: false,
            },
        );
        snap.market_order_meta.insert(
            992_014,
            MarketOrderMeta {
                issued_unix: now,
                duration_days: 30,
                range: "station".into(),
                issued_by: None,
                wallet_division: Some(1),
                is_corporation: true,
                state: Some("expired".into()),
            },
        );

        // Public expired contract (for 204 no_content on public items/bids)
        snap.contracts.insert(
            992_100,
            ContractRecord {
                contract_id: 992_100,
                issuer_id: char_id,
                issuer_corp_id: 0,
                assignee_id: 0,
                acceptor_id: None,
                contract_type: "Auction".into(),
                availability: "Public".into(),
                status: "Completed".into(),
                title: "Done".into(),
                for_corp: true,
                price: IskCents::from_isk(100),
                reward: IskCents::ZERO,
                collateral: IskCents::ZERO,
                buyout: IskCents::from_isk(200),
                start_station_id: 60_003_760,
                end_station_id: 60_003_760,
                date_issued_unix: now - 100,
                num_days: 7,
                expiration_unix: now + 1000,
                item_ids: vec![],
                requested_items: vec![(34, 5)],
                current_bid: IskCents::from_isk(100),
                current_bidder_id: None,
                bid_history: vec![],
            },
        );
        if let Some(mut c3) = snap.characters.get(&90_000_002).cloned() {
            c3.character_id = 90_000_003;
            c3.user_id = 99_003;
            c3.name = "Third Pilot".into();
            snap.characters.insert(90_000_003, c3);
        }
    });

    // /markets/structures/{structure_id}
    assert_eq!(
        dispatch_economy_route(
            &ctx,
            "GET",
            "/markets/structures/{structure_id}",
            &params(&[("structure_id", "bad")]),
            &empty_q,
            Some(&claims),
            1
        )
        .unwrap()
        .status_code,
        400
    );
    assert_eq!(
        dispatch_economy_route(
            &ctx,
            "GET",
            "/markets/structures/{structure_id}",
            &params(&[("structure_id", "9999999999")]),
            &empty_q,
            Some(&claims),
            1
        )
        .unwrap()
        .status_code,
        404
    );
    // Denied ACL (no claims -> caller_char_id = 0)
    assert_eq!(
        dispatch_economy_route(
            &ctx,
            "GET",
            "/markets/structures/{structure_id}",
            &params(&[("structure_id", &fortizar_id.to_string())]),
            &empty_q,
            None,
            1
        )
        .unwrap()
        .status_code,
        403
    );
    // Allowed ACL -> 200
    let st_ok = dispatch_economy_route(
        &ctx,
        "GET",
        "/markets/structures/{structure_id}",
        &params(&[("structure_id", &fortizar_id.to_string())]),
        &empty_q,
        Some(&claims),
        1,
    )
    .unwrap();
    assert_eq!(st_ok.status_code, 200);
    assert_eq!(body(&st_ok).as_array().unwrap().len(), 1);
    assert_eq!(
        dispatch_economy_route(
            &ctx,
            "GET",
            "/markets/structures/{structure_id}",
            &params(&[("structure_id", &fortizar_id.to_string())]),
            &empty_q,
            Some(&claims),
            999
        )
        .unwrap()
        .status_code,
        404
    );

    // Char & Corp orders / history: 400, 404, pagination
    let p_bad_char = params(&[("character_id", "0")]);
    let p_miss_char = params(&[("character_id", "99999999")]);
    let p_char = params(&[("character_id", &char_id.to_string())]);
    let p_bad_corp = params(&[("corporation_id", "0")]);
    let p_miss_corp = params(&[("corporation_id", "99999999")]);
    let p_corp = params(&[("corporation_id", &corp_id.to_string())]);

    for path in [
        "/characters/{character_id}/orders",
        "/characters/{character_id}/orders/history",
        "/characters/{character_id}/contracts",
    ] {
        assert_eq!(
            dispatch_economy_route(&ctx, "GET", path, &p_bad_char, &empty_q, None, 1)
                .unwrap()
                .status_code,
            400
        );
        assert_eq!(
            dispatch_economy_route(&ctx, "GET", path, &p_miss_char, &empty_q, None, 1)
                .unwrap()
                .status_code,
            404
        );
    }
    for path in [
        "/corporations/{corporation_id}/orders",
        "/corporations/{corporation_id}/orders/history",
        "/corporations/{corporation_id}/contracts",
    ] {
        assert_eq!(
            dispatch_economy_route(&ctx, "GET", path, &p_bad_corp, &empty_q, None, 1)
                .unwrap()
                .status_code,
            400
        );
        assert_eq!(
            dispatch_economy_route(&ctx, "GET", path, &p_miss_corp, &empty_q, None, 1)
                .unwrap()
                .status_code,
            404
        );
    }

    assert_eq!(
        dispatch_economy_route(
            &ctx,
            "GET",
            "/characters/{character_id}/orders/history",
            &p_char,
            &empty_q,
            None,
            999
        )
        .unwrap()
        .status_code,
        404
    );
    assert_eq!(
        dispatch_economy_route(
            &ctx,
            "GET",
            "/corporations/{corporation_id}/orders",
            &p_corp,
            &empty_q,
            None,
            999
        )
        .unwrap()
        .status_code,
        404
    );
    assert_eq!(
        dispatch_economy_route(
            &ctx,
            "GET",
            "/corporations/{corporation_id}/orders/history",
            &p_corp,
            &empty_q,
            None,
            1
        )
        .unwrap()
        .status_code,
        200
    );
    assert_eq!(
        dispatch_economy_route(
            &ctx,
            "GET",
            "/corporations/{corporation_id}/orders/history",
            &p_corp,
            &empty_q,
            None,
            999
        )
        .unwrap()
        .status_code,
        404
    );

    // Contracts error paths: 400, 403, 404, 204
    for path in [
        "/characters/{character_id}/contracts/{contract_id}/items",
        "/characters/{character_id}/contracts/{contract_id}/bids",
    ] {
        assert_eq!(
            dispatch_economy_route(
                &ctx,
                "GET",
                path,
                &params(&[("character_id", "bad"), ("contract_id", "66001")]),
                &empty_q,
                None,
                1
            )
            .unwrap()
            .status_code,
            400
        );
        assert_eq!(
            dispatch_economy_route(
                &ctx,
                "GET",
                path,
                &params(&[("character_id", &char_id.to_string()), ("contract_id", "bad")]),
                &empty_q,
                None,
                1
            )
            .unwrap()
            .status_code,
            400
        );
        assert_eq!(
            dispatch_economy_route(
                &ctx,
                "GET",
                path,
                &params(&[("character_id", "99999999"), ("contract_id", "66001")]),
                &empty_q,
                None,
                1
            )
            .unwrap()
            .status_code,
            404
        );
        assert_eq!(
            dispatch_economy_route(
                &ctx,
                "GET",
                path,
                &params(&[("character_id", &char_id.to_string()), ("contract_id", "999999")]),
                &empty_q,
                None,
                1
            )
            .unwrap()
            .status_code,
            404
        );
        assert_eq!(
            dispatch_economy_route(
                &ctx,
                "GET",
                path,
                &params(&[("character_id", "90000003"), ("contract_id", "66001")]),
                &empty_q,
                None,
                1
            )
            .unwrap()
            .status_code,
            403
        );
    }

    assert_eq!(
        dispatch_economy_route(
            &ctx,
            "GET",
            "/contracts/public/{region_id}",
            &params(&[("region_id", "bad")]),
            &empty_q,
            None,
            1
        )
        .unwrap()
        .status_code,
        400
    );
    assert_eq!(
        dispatch_economy_route(
            &ctx,
            "GET",
            "/contracts/public/{region_id}",
            &params(&[("region_id", "999999999999")]),
            &empty_q,
            None,
            1
        )
        .unwrap()
        .status_code,
        404
    );
    assert_eq!(
        dispatch_economy_route(
            &ctx,
            "GET",
            "/contracts/public/{region_id}",
            &params(&[("region_id", "19999999")]),
            &empty_q,
            None,
            1
        )
        .unwrap()
        .status_code,
        404
    );

    for path in [
        "/contracts/public/items/{contract_id}",
        "/contracts/public/bids/{contract_id}",
    ] {
        assert_eq!(
            dispatch_economy_route(
                &ctx,
                "GET",
                path,
                &params(&[("contract_id", "bad")]),
                &empty_q,
                None,
                1
            )
            .unwrap()
            .status_code,
            400
        );
        assert_eq!(
            dispatch_economy_route(
                &ctx,
                "GET",
                path,
                &params(&[("contract_id", "999999")]),
                &empty_q,
                None,
                1
            )
            .unwrap()
            .status_code,
            404
        );
        assert_eq!(
            dispatch_economy_route(
                &ctx,
                "GET",
                path,
                &params(&[("contract_id", "992100")]),
                &empty_q,
                None,
                1
            )
            .unwrap()
            .status_code,
            204
        );
        assert_eq!(
            dispatch_economy_route(
                &ctx,
                "GET",
                path,
                &params(&[("contract_id", "66001")]),
                &empty_q,
                None,
                999
            )
            .unwrap()
            .status_code,
            404
        );
    }

    for path in [
        "/corporations/{corporation_id}/contracts/{contract_id}/items",
        "/corporations/{corporation_id}/contracts/{contract_id}/bids",
    ] {
        assert_eq!(
            dispatch_economy_route(
                &ctx,
                "GET",
                path,
                &params(&[("corporation_id", "bad"), ("contract_id", "66001")]),
                &empty_q,
                None,
                1
            )
            .unwrap()
            .status_code,
            400
        );
        assert_eq!(
            dispatch_economy_route(
                &ctx,
                "GET",
                path,
                &params(&[("corporation_id", &corp_id.to_string()), ("contract_id", "bad")]),
                &empty_q,
                None,
                1
            )
            .unwrap()
            .status_code,
            400
        );
        assert_eq!(
            dispatch_economy_route(
                &ctx,
                "GET",
                path,
                &params(&[("corporation_id", "99999999"), ("contract_id", "66001")]),
                &empty_q,
                None,
                1
            )
            .unwrap()
            .status_code,
            404
        );
        assert_eq!(
            dispatch_economy_route(
                &ctx,
                "GET",
                path,
                &params(&[("corporation_id", &corp_id.to_string()), ("contract_id", "999999")]),
                &empty_q,
                None,
                1
            )
            .unwrap()
            .status_code,
            404
        );
    }

    // Corp contract with issuer_corp_id == 0 (992100) returns 200 on corp contracts
    let corp_c_list = dispatch_economy_route(
        &ctx,
        "GET",
        "/corporations/{corporation_id}/contracts",
        &p_corp,
        &empty_q,
        None,
        1,
    )
    .unwrap();
    assert_eq!(corp_c_list.status_code, 200);
    assert!(body(&corp_c_list)
        .as_array()
        .unwrap()
        .iter()
        .any(|c| c["contract_id"] == 992_100 && c["issuer_corporation_id"] == corp_id));
}

#[test]
fn mutant_kill_economy_unknown_route_structures_and_contract_in_region() {
    let (ctx, _token, char_id, corp_id, _planet_id) = seed_full_esi_context();
    let now = ctx.now_unix();
    let empty_q = HashMap::new();
    let claims = make_claims(char_id);

    // Unknown route returns None (kills dispatch_economy_route -> Some(Default::default()))
    assert!(
        dispatch_economy_route(&ctx, "GET", "/nonexistent/economy/route", &params(&[]), &empty_q, None, 1)
            .is_none()
    );

    // Find a solar system outside The Forge (10_000_002) for negative contract_in_region coverage
    let foreign_sys = ctx
        .sde
        .solar_systems
        .values()
        .find(|s| s.region_id != 10_000_002)
        .map(|s| s.solar_system_id)
        .unwrap_or(30_002_187);

    let fortizar_id = 1_055_099_001_i64;
    let foreign_struct_id = 1_055_099_002_i64;
    let contract_in_id = 993_101_i64;
    let contract_out_id = 993_102_i64;

    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            fortizar_id,
            UpwellStructureRecord {
                structure_id: fortizar_id,
                solar_system_id: 30_000_142,
                owner_corp_id: corp_id,
                type_id: 35_833,
                name: "Test Market Fortizar".into(),
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 500,
                ..Default::default()
            },
        );
        snap.items.insert(
            993_001,
            InventoryItemRecord {
                item_id: 993_001,
                type_id: 35_892,
                owner_id: corp_id,
                location_id: fortizar_id,
                flag: InventoryFlag::ServiceSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.module_online_states.insert(993_001, true);
        snap.structures.insert(
            foreign_struct_id,
            UpwellStructureRecord {
                structure_id: foreign_struct_id,
                solar_system_id: foreign_sys,
                owner_corp_id: corp_id,
                type_id: 35_833,
                name: "Foreign Fortizar".into(),
                state: "ShieldVulnerable".into(),
                ..Default::default()
            },
        );
        snap.market_orders.insert(
            880_900_001,
            MarketOrderRecord {
                order_id: 880_900_001,
                type_id: 34,
                owner_id: char_id,
                station_id: fortizar_id,
                region_id: 10_000_002,
                is_bid: false,
                price: IskCents::from_isk(12),
                vol_entered: 100,
                vol_remaining: 80,
                min_volume: 1,
                escrow: IskCents::ZERO,
                is_npc: false,
            },
        );
        snap.market_order_meta.insert(
            880_900_001,
            MarketOrderMeta {
                issued_unix: now - 60,
                duration_days: 90,
                range: "station".into(),
                issued_by: Some(char_id),
                wallet_division: Some(1),
                is_corporation: false,
                state: None,
            },
        );

        let make_pub_contract = |cid: i64, start_loc: i64| ContractRecord {
            contract_id: cid,
            issuer_id: char_id,
            issuer_corp_id: corp_id,
            assignee_id: 0,
            acceptor_id: None,
            contract_type: "ItemExchange".into(),
            availability: "Public".into(),
            status: "Outstanding".into(),
            title: format!("Pub {cid}"),
            for_corp: false,
            price: IskCents::from_isk(5_000),
            reward: IskCents::ZERO,
            collateral: IskCents::ZERO,
            buyout: IskCents::ZERO,
            start_station_id: start_loc,
            end_station_id: start_loc,
            date_issued_unix: now - 10,
            num_days: 7,
            expiration_unix: now + 86_400,
            item_ids: vec![],
            requested_items: vec![],
            current_bid: IskCents::ZERO,
            current_bidder_id: None,
            bid_history: vec![],
        };
        snap.contracts.insert(contract_in_id, make_pub_contract(contract_in_id, 60_003_760));
        snap.contracts.insert(contract_out_id, make_pub_contract(contract_out_id, foreign_struct_id));
    });

    // /markets/structures/{structure_id} verified on fields
    let st_res = dispatch_economy_route(
        &ctx,
        "GET",
        "/markets/structures/{structure_id}",
        &params(&[("structure_id", &fortizar_id.to_string())]),
        &empty_q,
        Some(&claims),
        1,
    )
    .unwrap();
    assert_eq!(st_res.status_code, 200);
    let st_rows = body(&st_res).as_array().unwrap();
    assert_eq!(st_rows.len(), 1);
    assert_eq!(st_rows[0]["order_id"], 880_900_001);
    assert_eq!(st_rows[0]["location_id"], fortizar_id);
    assert_eq!(st_rows[0]["volume_remain"], 80);

    // /contracts/public/{region_id} includes The Forge contract and excludes foreign-region contract
    let pub_res = dispatch_economy_route(
        &ctx,
        "GET",
        "/contracts/public/{region_id}",
        &params(&[("region_id", "10000002")]),
        &empty_q,
        None,
        1,
    )
    .unwrap();
    assert_eq!(pub_res.status_code, 200);
    let pub_rows = body(&pub_res).as_array().unwrap();
    assert!(
        pub_rows.iter().any(|c| c["contract_id"] == contract_in_id),
        "expected Jita public contract in region 10000002"
    );
    assert!(
        !pub_rows.iter().any(|c| c["contract_id"] == contract_out_id),
        "foreign-region public contract must be excluded by contract_in_region"
    );
}
