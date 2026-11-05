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
            c.assignee_id = char_id;
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
