//! Unit and mutation-killing tests for `esi::economy_routes`.

use super::*;
use crate::esi::full_endpoint_catalog_tests::seed_full_esi_context;

fn params(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
        .collect()
}

fn body(res: &RouteExecutionResult) -> &Value {
    res.body.as_ref().expect("response body")
}

fn sample_contract(id: i64, issuer_id: i64, corp_id: i64) -> eve_db::ContractRecord {
    eve_db::ContractRecord {
        contract_id: id,
        issuer_id,
        issuer_corp_id: corp_id,
        assignee_id: 0,
        acceptor_id: None,
        contract_type: "ItemExchange".to_string(),
        availability: "Public".to_string(),
        status: "Outstanding".to_string(),
        title: "Test Contract".to_string(),
        for_corp: false,
        price: eve_db::IskCents::from_isk(1_000_000),
        reward: eve_db::IskCents::ZERO,
        collateral: eve_db::IskCents::ZERO,
        buyout: eve_db::IskCents::from_isk(2_000_000),
        start_station_id: 60_003_760,
        end_station_id: 60_003_760,
        date_issued_unix: 1_700_000_000,
        num_days: 7,
        expiration_unix: 1_700_604_800,
        item_ids: vec![],
        requested_items: vec![(34, 50)],
        current_bid: eve_db::IskCents::from_isk(1_200_000),
        current_bidder_id: Some(90_000_002),
        bid_history: vec![
            (
                90_000_003,
                eve_db::IskCents::from_isk(1_100_000),
                1_700_001_000,
            ),
            (
                90_000_002,
                eve_db::IskCents::from_isk(1_200_000),
                1_700_002_000,
            ),
        ],
    }
}

#[test]
fn contract_mappers_expiry_retention_party_region_items_and_bids() {
    let empty = HashMap::new();
    assert_eq!(
        parse_path_i64(&empty, "region_id")
            .expect_err("missing")
            .status_code,
        400
    );
    assert_eq!(
        parse_path_i64(&params(&[("region_id", "x")]), "region_id")
            .expect_err("nan")
            .status_code,
        400
    );
    assert_eq!(
        parse_path_i64(&params(&[("region_id", "0")]), "region_id")
            .expect_err("zero")
            .status_code,
        400
    );
    assert_eq!(
        parse_path_i64(&params(&[("region_id", "-1")]), "region_id")
            .expect_err("neg")
            .status_code,
        400
    );
    assert_eq!(
        parse_path_i64(&params(&[("region_id", "10000002")]), "region_id").unwrap(),
        10_000_002
    );

    for (raw, exp) in [
        ("ItemExchange", "item_exchange"),
        ("item_exchange", "item_exchange"),
        ("Courier", "courier"),
        ("courier", "courier"),
        ("Auction", "auction"),
        ("auction", "auction"),
        ("Loan", "loan"),
        ("loan", "loan"),
        ("Other", "unknown"),
    ] {
        assert_eq!(map_contract_type(raw), exp);
    }

    for (raw, exp) in [
        ("Outstanding", "outstanding"),
        ("InProgress", "in_progress"),
        ("Completed", "finished"),
        ("Failed", "failed"),
        ("Expired", "deleted"),
        ("Deleted", "deleted"),
        ("Rejected", "rejected"),
        ("Reversed", "reversed"),
        ("Cancelled", "cancelled"),
        ("Other", "outstanding"),
    ] {
        assert_eq!(map_contract_status(raw), exp);
    }

    assert_eq!(map_contract_availability("Personal"), "personal");
    assert_eq!(map_contract_availability("Corporation"), "corporation");
    assert_eq!(map_contract_availability("Alliance"), "alliance");
    assert_eq!(map_contract_availability("Public"), "public");

    let mut c = sample_contract(10, 90_000_001, 98_000_001);
    assert!(!is_contract_expired(&c, 1_700_000_000));
    assert!(is_contract_expired(&c, 1_700_604_800));
    assert!(contract_retained_in_list(&c, 1_700_000_000 + 86_400));
    assert!(!contract_retained_in_list(
        &c,
        1_700_000_000 + CONTRACT_RETENTION_SECS + 10
    ));
    c.status = "InProgress".to_string();
    assert!(contract_retained_in_list(
        &c,
        1_700_000_000 + CONTRACT_RETENTION_SECS + 10
    ));

    let bids_priv = contract_bid_rows(&c, true);
    assert_eq!(bids_priv.len(), 2);
    assert_eq!(bids_priv[0]["bid_id"], 2);
    assert_eq!(bids_priv[0]["bidder_id"], 90_000_002);
    let bids_pub = contract_bid_rows(&c, false);
    assert!(bids_pub[0].get("bidder_id").is_none());

    let (ctx, _token, char_id, corp_id, _planet_id) = seed_full_esi_context();
    ctx.db.with_snapshot(|snap| {
        assert!(is_character_party_to_contract(snap, char_id, &c));
        assert!(is_character_party_to_contract(snap, 90_000_003, &c));
        assert!(contract_in_region(&ctx, snap, &c, 10_000_002));
        assert!(!contract_in_region(&ctx, snap, &c, 10_000_043));

        let items_priv = contract_item_rows(snap, &c, false);
        assert_eq!(items_priv.len(), 1);
        assert_eq!(items_priv[0]["is_included"], false);
        assert_eq!(items_priv[0]["is_singleton"], false);

        let corp = snap.corporations.get(&corp_id).unwrap();
        let mut corp_c = c.clone();
        corp_c.for_corp = true;
        assert!(is_corp_contract(corp, corp_id, &corp_c));
    });
}

#[test]
fn dispatch_market_groups_prices_orders_history_types_and_structures() {
    let (ctx, _token, char_id, corp_id, _planet_id) = seed_full_esi_context();
    let char_str = char_id.to_string();
    let corp_str = corp_id.to_string();
    let p_char = params(&[("character_id", &char_str)]);
    let p_corp = params(&[("corporation_id", &corp_str)]);
    let p_jita_reg = params(&[("region_id", "10000002")]);
    let empty_q = HashMap::new();

    let first_mg = *ctx.sde.market_groups.keys().next().unwrap();
    let mg_ok = dispatch_economy_route(
        &ctx,
        "GET",
        "/markets/groups/{market_group_id}",
        &params(&[("market_group_id", &first_mg.to_string())]),
        &empty_q,
        None,
        1,
    )
    .unwrap();
    assert_eq!(mg_ok.status_code, 200);
    assert_eq!(body(&mg_ok)["market_group_id"], first_mg as i64);

    assert_eq!(
        dispatch_economy_route(
            &ctx,
            "GET",
            "/markets/groups/{market_group_id}",
            &params(&[("market_group_id", "999999999")]),
            &empty_q,
            None,
            1
        )
        .unwrap()
        .status_code,
        404
    );

    assert_eq!(
        dispatch_economy_route(&ctx, "GET", "/markets/prices", &empty_q, &empty_q, None, 1)
            .unwrap()
            .status_code,
        200
    );

    for ot in ["all", "buy", "sell"] {
        let ord = dispatch_economy_route(
            &ctx,
            "GET",
            "/markets/{region_id}/orders",
            &p_jita_reg,
            &params(&[("order_type", ot), ("type_id", "34")]),
            None,
            1,
        )
        .unwrap();
        assert_eq!(ord.status_code, 200);
    }
    assert_eq!(
        dispatch_economy_route(
            &ctx,
            "GET",
            "/markets/{region_id}/orders",
            &p_jita_reg,
            &params(&[("order_type", "invalid_side")]),
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
            &p_jita_reg,
            &empty_q,
            None,
            1
        )
        .unwrap()
        .status_code,
        400
    );
    let hist = dispatch_economy_route(
        &ctx,
        "GET",
        "/markets/{region_id}/history",
        &p_jita_reg,
        &params(&[("type_id", "34")]),
        None,
        1,
    )
    .unwrap();
    assert_eq!(hist.status_code, 200);

    let types = dispatch_economy_route(
        &ctx,
        "GET",
        "/markets/{region_id}/types",
        &p_jita_reg,
        &empty_q,
        None,
        1,
    )
    .unwrap();
    assert_eq!(types.status_code, 200);

    assert_eq!(
        dispatch_economy_route(
            &ctx,
            "GET",
            "/markets/structures/{structure_id}",
            &params(&[("structure_id", "1055000001")]),
            &empty_q,
            None,
            1
        )
        .unwrap()
        .status_code,
        403
    );

    for (path, p) in [
        ("/characters/{character_id}/orders", &p_char),
        ("/characters/{character_id}/orders/history", &p_char),
        ("/corporations/{corporation_id}/orders", &p_corp),
        ("/corporations/{corporation_id}/orders/history", &p_corp),
    ] {
        let res = dispatch_economy_route(&ctx, "GET", path, p, &empty_q, None, 1).unwrap();
        assert_eq!(res.status_code, 200, "failed on {path}");
    }
}

#[test]
fn dispatch_char_corp_and_public_contracts_items_and_bids() {
    let (ctx, _token, char_id, corp_id, _planet_id) = seed_full_esi_context();
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.contracts.get_mut(&66_001) {
            c.bid_history
                .push((char_id, eve_db::IskCents::from_isk(125_000), 1_700_003_000));
        }
    });
    let char_str = char_id.to_string();
    let corp_str = corp_id.to_string();
    let empty_q = HashMap::new();

    for (path, p) in [
        (
            "/characters/{character_id}/contracts",
            params(&[("character_id", &char_str)]),
        ),
        (
            "/characters/{character_id}/contracts/{contract_id}/items",
            params(&[("character_id", &char_str), ("contract_id", "66001")]),
        ),
        (
            "/characters/{character_id}/contracts/{contract_id}/bids",
            params(&[("character_id", &char_str), ("contract_id", "66001")]),
        ),
        (
            "/contracts/public/{region_id}",
            params(&[("region_id", "10000002")]),
        ),
        (
            "/contracts/public/items/{contract_id}",
            params(&[("contract_id", "66001")]),
        ),
        (
            "/contracts/public/bids/{contract_id}",
            params(&[("contract_id", "66001")]),
        ),
        (
            "/corporations/{corporation_id}/contracts",
            params(&[("corporation_id", &corp_str)]),
        ),
        (
            "/corporations/{corporation_id}/contracts/{contract_id}/items",
            params(&[("corporation_id", &corp_str), ("contract_id", "66001")]),
        ),
        (
            "/corporations/{corporation_id}/contracts/{contract_id}/bids",
            params(&[("corporation_id", &corp_str), ("contract_id", "66001")]),
        ),
    ] {
        let res = dispatch_economy_route(&ctx, "GET", path, &p, &empty_q, None, 1).unwrap();
        assert_eq!(res.status_code, 200, "failed on {path}");
        assert!(body(&res).is_array(), "expected array on {path}");
    }
}

#[test]
fn economy_routes_errors_prices_localized_groups_structures_orders_and_contracts_edge_cases() {
    let (ctx, _token, char_id, corp_id, _planet_id) = seed_full_esi_context();
    let empty_p = HashMap::new();
    let empty_q = HashMap::new();
    let p_char = params(&[("character_id", &char_id.to_string())]);
    let p_corp = params(&[("corporation_id", &corp_id.to_string())]);
    let p_jita_reg = params(&[("region_id", "10000002")]);

    // 400 missing path params & 404 unknown IDs across all market & contract routes
    for path in [
        "/markets/groups/{market_group_id}",
        "/markets/{region_id}/orders",
        "/markets/{region_id}/history",
        "/markets/{region_id}/types",
        "/markets/structures/{structure_id}",
        "/characters/{character_id}/orders",
        "/characters/{character_id}/orders/history",
        "/corporations/{corporation_id}/orders",
        "/corporations/{corporation_id}/orders/history",
        "/characters/{character_id}/contracts",
        "/characters/{character_id}/contracts/{contract_id}/items",
        "/characters/{character_id}/contracts/{contract_id}/bids",
        "/contracts/public/{region_id}",
        "/contracts/public/items/{contract_id}",
        "/contracts/public/bids/{contract_id}",
        "/corporations/{corporation_id}/contracts",
        "/corporations/{corporation_id}/contracts/{contract_id}/items",
        "/corporations/{corporation_id}/contracts/{contract_id}/bids",
    ] {
        assert_eq!(
            dispatch_economy_route(&ctx, "GET", path, &empty_p, &empty_q, None, 1)
                .unwrap()
                .status_code,
            400,
            "missing param 400 on {path}"
        );
    }

    for (path, p) in [
        ("/markets/groups/{market_group_id}", params(&[("market_group_id", "9999999999")])),
        ("/markets/groups/{market_group_id}", params(&[("market_group_id", "99999999")])),
        ("/markets/{region_id}/orders", params(&[("region_id", "9999999999")])),
        ("/markets/{region_id}/orders", params(&[("region_id", "99999999")])),
        ("/markets/{region_id}/history", params(&[("region_id", "9999999999")])),
        ("/markets/{region_id}/history", params(&[("region_id", "99999999")])),
        ("/markets/{region_id}/types", params(&[("region_id", "9999999999")])),
        ("/markets/{region_id}/types", params(&[("region_id", "99999999")])),
        ("/markets/structures/{structure_id}", params(&[("structure_id", "99999999")])),
        ("/characters/{character_id}/orders", params(&[("character_id", "90999666")])),
        ("/characters/{character_id}/orders/history", params(&[("character_id", "90999666")])),
        ("/corporations/{corporation_id}/orders", params(&[("corporation_id", "98999666")])),
        ("/corporations/{corporation_id}/orders/history", params(&[("corporation_id", "98999666")])),
        ("/characters/{character_id}/contracts", params(&[("character_id", "90999666")])),
        ("/characters/{character_id}/contracts/{contract_id}/items", params(&[("character_id", "90999666"), ("contract_id", "66001")])),
        ("/characters/{character_id}/contracts/{contract_id}/items", params(&[("character_id", &char_id.to_string()), ("contract_id", "999999")])),
        ("/characters/{character_id}/contracts/{contract_id}/bids", params(&[("character_id", "90999666"), ("contract_id", "66001")])),
        ("/characters/{character_id}/contracts/{contract_id}/bids", params(&[("character_id", &char_id.to_string()), ("contract_id", "999999")])),
        ("/contracts/public/{region_id}", params(&[("region_id", "9999999999")])),
        ("/contracts/public/{region_id}", params(&[("region_id", "99999999")])),
        ("/contracts/public/items/{contract_id}", params(&[("contract_id", "999999")])),
        ("/contracts/public/bids/{contract_id}", params(&[("contract_id", "999999")])),
        ("/corporations/{corporation_id}/contracts", params(&[("corporation_id", "98999666")])),
        ("/corporations/{corporation_id}/contracts/{contract_id}/items", params(&[("corporation_id", "98999666"), ("contract_id", "66001")])),
        ("/corporations/{corporation_id}/contracts/{contract_id}/items", params(&[("corporation_id", &corp_id.to_string()), ("contract_id", "999999")])),
        ("/corporations/{corporation_id}/contracts/{contract_id}/bids", params(&[("corporation_id", "98999666"), ("contract_id", "66001")])),
        ("/corporations/{corporation_id}/contracts/{contract_id}/bids", params(&[("corporation_id", &corp_id.to_string()), ("contract_id", "999999")])),
    ] {
        assert_eq!(
            dispatch_economy_route(&ctx, "GET", path, &p, &empty_q, None, 1)
                .unwrap()
                .status_code,
            404,
            "404 expected on {path} with {p:?}"
        );
    }

    // Localized market group + history query errors + invalid order query
    let any_mg = *ctx.sde.market_groups.keys().next().expect("mg");
    assert_eq!(
        dispatch_economy_route(
            &ctx,
            "GET",
            "/markets/groups/{market_group_id}",
            &params(&[("market_group_id", &any_mg.to_string())]),
            &params(&[("language", "de")]),
            None,
            1,
        )
        .unwrap()
        .status_code,
        200
    );
    assert_eq!(
        dispatch_economy_route(
            &ctx,
            "GET",
            "/markets/{region_id}/orders",
            &p_jita_reg,
            &params(&[("type_id", "0")]),
            None,
            1,
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
            &p_jita_reg,
            &params(&[("type_id", "bad")]),
            None,
            1,
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
            &p_jita_reg,
            &params(&[("type_id", "99999999")]),
            None,
            1,
        )
        .unwrap()
        .status_code,
        404
    );

    // Seed Upwell market structure (with Standup Market Hub online), zero-volume history + price branches,
    // incomplete order metas, non-party character, expired public contract, and structure-based public contract
    let mkt_struct_id = 1_055_000_099_i64;
    let mkt_svc_id = 886_001_i64;
    let zero_vol_tid = 36_u32;
    let adj_only_tid = 37_u32;
    let order_only_tid = 38_u32;
    let outsider_id = 90_886_001_i64;

    ctx.db.with_mut_snapshot(|snap| {
        let mut och = snap.characters[&char_id].clone();
        och.character_id = outsider_id;
        och.corporation_id = 98_999_111;
        och.alliance_id = None;
        snap.characters.insert(outsider_id, och);

        // Zero-volume market history (covers lines 348-350 in /markets/prices)
        snap.market_history.insert(
            (10_000_002, zero_vol_tid),
            vec![eve_db::snapshot_ext::MarketHistoryDayRecord {
                date: "2026-11-01".to_string(),
                average: 55.0,
                highest: 60.0,
                lowest: 50.0,
                order_count: 1,
                volume: 0,
                open_price: None,
                close_price: None,
            }],
        );
        // Live adjusted index item via market history for adj_only_tid in another region (covers live_adjusted without market_history for 10_000_002 or adjusted-only fallback)
        snap.market_history.insert(
            (10_000_043, adj_only_tid),
            vec![eve_db::snapshot_ext::MarketHistoryDayRecord {
                date: "2026-11-01".to_string(),
                average: 123.45,
                highest: 130.0,
                lowest: 120.0,
                order_count: 1,
                volume: 0,
                open_price: None,
                close_price: None,
            }],
        );
        // Live order without market_history or live_adjusted (covers lines 376-395)
        snap.market_orders.insert(
            990_001,
            eve_db::MarketOrderRecord {
                order_id: 990_001,
                type_id: order_only_tid,
                owner_id: char_id,
                station_id: 60_003_760,
                region_id: 10_000_002,
                price: eve_db::IskCents(1000),
                vol_entered: 10,
                vol_remaining: 5,
                min_volume: 1,
                is_bid: false,
                is_npc: false,
                escrow: eve_db::IskCents(0),
            },
        );
        // Corp order missing meta / state / issued_by (lines 788-799, 847-858)
        snap.market_orders.insert(
            990_002,
            eve_db::MarketOrderRecord {
                order_id: 990_002,
                type_id: 34,
                owner_id: corp_id,
                station_id: 60_003_760,
                region_id: 10_000_002,
                price: eve_db::IskCents(1000),
                vol_entered: 10,
                vol_remaining: 5,
                min_volume: 1,
                is_bid: false,
                is_npc: false,
                escrow: eve_db::IskCents(0),
            },
        );
        // Upwell Fortizar with online Standup Market Hub (35_892)
        snap.structures.insert(
            mkt_struct_id,
            eve_db::UpwellStructureRecord {
                structure_id: mkt_struct_id,
                type_id: 35833,
                owner_corp_id: corp_id,
                solar_system_id: 30_000_142,
                name: "Mkt Fort".to_string(),
                state: "ShieldVulnerable".to_string(),
                quantum_core_installed: true,
                fuel_blocks: 100,
                ..Default::default()
            },
        );
        snap.items.insert(
            mkt_svc_id,
            eve_db::InventoryItemRecord {
                item_id: mkt_svc_id,
                type_id: 35_892,
                owner_id: corp_id,
                location_id: mkt_struct_id,
                flag: eve_db::InventoryFlag::ServiceSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.module_online_states.insert(mkt_svc_id, true);
        snap.market_orders.insert(
            990_003,
            eve_db::MarketOrderRecord {
                order_id: 990_003,
                type_id: 34,
                owner_id: corp_id,
                station_id: mkt_struct_id,
                region_id: 10_000_002,
                price: eve_db::IskCents(1200),
                vol_entered: 10,
                vol_remaining: 8,
                min_volume: 1,
                is_bid: true,
                is_npc: false,
                escrow: eve_db::IskCents(9600),
            },
        );
        snap.market_order_meta.insert(
            990_003,
            eve_db::MarketOrderMeta {
                duration_days: 90,
                issued_unix: 1_700_000_000,
                range: "station".to_string(),
                is_corporation: true,
                issued_by: Some(char_id),
                wallet_division: Some(1),
                state: Some("open".to_string()),
            },
        );

        // Expired public contract + structure-based active public contract + for_corp w/ issuer_corp_id=0
        snap.contracts.insert(
            66_901,
            eve_db::ContractRecord {
                contract_id: 66_901,
                issuer_id: char_id,
                contract_type: "Auction".to_string(),
                status: "Expired".to_string(),
                start_station_id: mkt_struct_id,
                end_station_id: mkt_struct_id,
                availability: "Public".to_string(),
                expiration_unix: 1,
                date_issued_unix: 0,
                ..Default::default()
            },
        );
        snap.contracts.insert(
            66_902,
            eve_db::ContractRecord {
                contract_id: 66_902,
                issuer_id: char_id,
                contract_type: "ItemExchange".to_string(),
                status: "Outstanding".to_string(),
                start_station_id: mkt_struct_id,
                end_station_id: mkt_struct_id,
                availability: "Public".to_string(),
                expiration_unix: 0,
                date_issued_unix: 0,
                issuer_corp_id: 0,
                for_corp: true,
                item_ids: vec![999_888_777], // missing item -> continue line 183
                ..Default::default()
            },
        );
    });

    let prices = dispatch_economy_route(&ctx, "GET", "/markets/prices", &empty_p, &empty_q, None, 1).unwrap();
    assert_eq!(prices.status_code, 200);
    let p_arr = body(&prices).as_array().unwrap();
    assert!(p_arr.iter().any(|r| r["type_id"] == i64::from(zero_vol_tid)));
    assert!(p_arr.iter().any(|r| r["type_id"] == i64::from(adj_only_tid)));
    assert!(p_arr.iter().any(|r| r["type_id"] == i64::from(order_only_tid)));

    // Market structure ACL denied for outsider (403) vs allowed for corp member (200) & page 999 (404)
    let c_out = SsoTokenClaims {
        sub: format!("CHARACTER:EVE:{outsider_id}"),
        name: "Out".to_string(),
        owner: "h".to_string(),
        user_id: 88,
        character_id: outsider_id,
        scp: Vec::new(),
        iss: "e".to_string(),
        aud: vec!["EVE Online".to_string()],
        iat: 1,
        exp: 2_000_000_000,
        jti: "j".to_string(),
    };
    let c_in = SsoTokenClaims {
        character_id: char_id,
        ..c_out.clone()
    };
    let p_mkt_st = params(&[("structure_id", &mkt_struct_id.to_string())]);
    assert_eq!(
        dispatch_economy_route(&ctx, "GET", "/markets/structures/{structure_id}", &p_mkt_st, &empty_q, Some(&c_out), 1)
            .unwrap()
            .status_code,
        403
    );
    let st_ords = dispatch_economy_route(&ctx, "GET", "/markets/structures/{structure_id}", &p_mkt_st, &empty_q, Some(&c_in), 1).unwrap();
    assert_eq!(st_ords.status_code, 200);
    assert_eq!(body(&st_ords).as_array().unwrap().len(), 1);

    // Pagination 404 across paginated endpoints
    for (path, p) in [
        ("/markets/{region_id}/orders", p_jita_reg.clone()),
        ("/markets/{region_id}/types", p_jita_reg.clone()),
        ("/markets/structures/{structure_id}", p_mkt_st),
        ("/characters/{character_id}/orders/history", p_char.clone()),
        ("/corporations/{corporation_id}/orders", p_corp.clone()),
        ("/corporations/{corporation_id}/orders/history", p_corp.clone()),
        ("/characters/{character_id}/contracts", p_char),
        ("/contracts/public/{region_id}", p_jita_reg),
        ("/contracts/public/items/{contract_id}", params(&[("contract_id", "66902")])),
        ("/contracts/public/bids/{contract_id}", params(&[("contract_id", "66902")])),
        ("/corporations/{corporation_id}/contracts", p_corp.clone()),
        ("/corporations/{corporation_id}/contracts/{contract_id}/bids", params(&[("corporation_id", &corp_id.to_string()), ("contract_id", "66001")])),
    ] {
        assert_eq!(
            dispatch_economy_route(&ctx, "GET", path, &p, &empty_q, Some(&c_in), 999)
                .unwrap()
                .status_code,
            404,
            "page 999 404 on {path}"
        );
    }

    // Expired public contract items/bids return 204 no_content
    assert_eq!(
        dispatch_economy_route(&ctx, "GET", "/contracts/public/items/{contract_id}", &params(&[("contract_id", "66901")]), &empty_q, None, 1)
            .unwrap()
            .status_code,
        204
    );
    assert_eq!(
        dispatch_economy_route(&ctx, "GET", "/contracts/public/bids/{contract_id}", &params(&[("contract_id", "66901")]), &empty_q, None, 1)
            .unwrap()
            .status_code,
        204
    );

    // Non-party character on /characters/{outsider}/contracts/66001/{items,bids} -> 403
    assert_eq!(
        dispatch_economy_route(
            &ctx,
            "GET",
            "/characters/{character_id}/contracts/{contract_id}/items",
            &params(&[("character_id", &outsider_id.to_string()), ("contract_id", "66001")]),
            &empty_q,
            None,
            1,
        )
        .unwrap()
        .status_code,
        403
    );
    assert_eq!(
        dispatch_economy_route(
            &ctx,
            "GET",
            "/characters/{character_id}/contracts/{contract_id}/bids",
            &params(&[("character_id", &outsider_id.to_string()), ("contract_id", "66001")]),
            &empty_q,
            None,
            1,
        )
        .unwrap()
        .status_code,
        403
    );
}
