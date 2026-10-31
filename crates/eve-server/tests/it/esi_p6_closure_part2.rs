//! `ESI-6` live HTTP + RPC + SQLite persistence regression tests for contract party isolation,
//! public contract `204` lifecycle transitions, and 30-day contract list retention.

#![forbid(unsafe_code)]

use crate::esi_market_industry_pi::{assert_status, get_json, issue_token_with_scopes};
use eve_db::{
    corp_roles::{ROLE_CONTRACT_MANAGER, ROLE_DIRECTOR},
    CorporationRecord, InventoryFlag, InventoryItemRecord, IskCents, WorldRepository,
};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::esi::{EsiHttpRequest, EsiRouter, OpenApiSpec};
use eve_server::router::ServerContext;
use std::collections::HashMap;
use std::sync::Arc;

fn temp_sqlite_path(tag: &str) -> std::path::PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    std::env::temp_dir().join(format!("evomu_p6_esi1_pt2_{tag}_{nanos}.sqlite"))
}

fn open_seeded_sqlite_ctx(tag: &str) -> (std::path::PathBuf, Arc<ServerContext>) {
    let db_path = temp_sqlite_path(tag);
    let repo = Arc::new(WorldRepository::open_sqlite(&db_path).expect("open sqlite"));
    let ctx = Arc::new(ServerContext::with_repository(repo));
    let demo = WorldRepository::with_demo_seed().with_snapshot(|s| s.clone());
    ctx.db
        .try_with_mut_snapshot(|snap| {
            *snap = demo;
            Ok::<_, std::convert::Infallible>(())
        })
        .unwrap();
    crate::common::pin_clock(&ctx);
    (db_path, ctx)
}

fn setup_session(
    ctx: &ServerContext,
    char_id: i64,
    corp_id: i64,
    station_id: i64,
    roles: u64,
) -> SessionState {
    let mut sess = SessionState::default();
    sess.user_id = Some(1);
    sess.character_id = Some(char_id);
    sess.corporation_id = Some(corp_id);
    sess.station_id = Some(station_id);
    sess.solar_system_id = Some(30_000_142);
    sess.region_id = Some(10_000_002);
    sess.corp_role = roles;
    let _: Result<(), ()> = ctx.db.try_with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.corporation_id = corp_id;
            ch.station_id = Some(station_id);
            ch.solar_system_id = 30_000_142;
            ch.balance = IskCents::from_isk(500_000_000);
        }
        if let Some(corp) = snap.corporations.get_mut(&corp_id) {
            corp.members.insert(char_id);
            corp.member_roles.insert(char_id, roles);
        }
        Ok(())
    });
    sess
}

fn seed_hangar_item(
    ctx: &ServerContext,
    owner_id: i64,
    station_id: i64,
    type_id: u32,
    qty: i32,
) -> i64 {
    ctx.db
        .try_with_mut_snapshot(|snap| {
            snap.next_entity_id += 1;
            let iid = snap.next_entity_id;
            snap.items.insert(
                iid,
                InventoryItemRecord {
                    item_id: iid,
                    type_id,
                    owner_id,
                    location_id: station_id,
                    flag: InventoryFlag::Hangar,
                    quantity: qty,
                    singleton: false,
                    version: 1,
                    ..Default::default()
                },
            );
            Ok::<_, std::convert::Infallible>(iid)
        })
        .unwrap()
}

#[allow(clippy::too_many_arguments)]
async fn create_contract_rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    ctype: i64,
    num_days: i64,
    start_station: i64,
    end_station: i64,
    price: f64,
    reward: f64,
    collateral_or_buyout: f64,
    title: &str,
    item_id: i64,
    qty: i64,
    for_corp: bool,
) -> i64 {
    let mut kw = HashMap::new();
    if for_corp {
        kw.insert("forCorp".to_string(), PyRep::Bool(true));
    }
    let PyRep::KeyVal(kv) = ctx
        .dispatch_rpc(
            sess,
            "contractProxy",
            None,
            "CreateContract",
            &[
                PyRep::Integer(ctype),
                PyRep::Integer(0),
                PyRep::Integer(0),
                PyRep::Integer(1440),
                PyRep::Integer(num_days),
                PyRep::Integer(start_station),
                PyRep::Integer(end_station),
                PyRep::Real(price),
                PyRep::Real(reward),
                PyRep::Real(collateral_or_buyout),
                PyRep::String(title.into()),
                PyRep::String(String::new()),
                PyRep::List(vec![PyRep::Tuple(vec![
                    PyRep::Integer(item_id),
                    PyRep::Integer(qty),
                ])]),
            ],
            &kw,
            None,
        )
        .await
        .expect("CreateContract")
    else {
        panic!("expected KeyVal from CreateContract");
    };
    kv.get("contractID").unwrap().as_i64().unwrap()
}

fn assert_204_valid(router: &EsiRouter, spec: &OpenApiSpec, target: impl Into<String>) {
    let req = EsiHttpRequest::new("GET", target);
    let resp = router.handle_request(&req);
    let raw_path = req.raw_target.split('?').next().unwrap_or(&req.raw_target);
    assert_eq!(
        resp.status_code,
        204,
        "Expected 204 for GET {}, got {} ({})",
        req.raw_target,
        resp.status_code,
        String::from_utf8_lossy(&resp.body)
    );
    assert!(resp.body.is_empty(), "204 response body must be empty");
    spec.validate_response("GET", raw_path, 204, &resp.headers, &resp.body)
        .unwrap_or_else(|e| panic!("OpenAPI 204 validation failed for {}: {e}", req.raw_target));
}

// parity: esi:GET /characters/{character_id}/contracts/{contract_id}/bids, esi:GET /characters/{character_id}/contracts/{contract_id}/items, esi:GET /contracts/public/bids/{contract_id}, esi:GET /contracts/public/items/{contract_id}, esi:GET /corporations/{corporation_id}/contracts/{contract_id}/bids, esi:GET /corporations/{corporation_id}/contracts/{contract_id}/items
#[tokio::test]
async fn test_esi_p6_char_contract_party_isolation_and_bidder_privacy() {
    let (db_path, ctx) = open_seeded_sqlite_ctx("party_privacy");
    let router = EsiRouter::new(ctx.clone());
    let spec = OpenApiSpec::pinned();
    let empty = HashMap::new();

    let station_id = 60_003_760_i64;
    let corp1 = 98_000_501_i64;
    let char3 = 90_000_888_i64;
    let corp3 = 98_000_503_i64;
    let (char1, char2, corp2) = ctx.db.with_snapshot(|snap| {
        let mut chars: Vec<_> = snap.characters.values().collect();
        chars.sort_by_key(|c| c.character_id);
        (
            chars[0].character_id,
            chars[1].character_id,
            chars[1].corporation_id,
        )
    });
    let _: Result<(), ()> = ctx.db.try_with_mut_snapshot(|snap| {
        let mut c1 = CorporationRecord {
            corporation_id: corp1,
            name: "Party Isolation Corp".into(),
            ticker: "PIC1".into(),
            ceo_id: char1,
            master_wallet: IskCents::from_isk(500_000_000),
            ..Default::default()
        };
        c1.ensure_wallet_divisions();
        snap.corporations.insert(corp1, c1);

        let c3 = CorporationRecord {
            corporation_id: corp3,
            name: "Third Party Corp".into(),
            ticker: "TPC3".into(),
            ceo_id: char3,
            master_wallet: IskCents::from_isk(100_000_000),
            ..Default::default()
        };
        snap.corporations.insert(corp3, c3);
        let mut bystander = snap.characters[&char1].clone();
        bystander.character_id = char3;
        bystander.name = "Unrelated Bystander".into();
        bystander.corporation_id = corp3;
        bystander.station_id = Some(station_id);
        bystander.balance = IskCents::from_isk(100_000_000);
        snap.characters.insert(char3, bystander);
        Ok(())
    });

    let mut sess1 = setup_session(
        &ctx,
        char1,
        corp1,
        station_id,
        ROLE_CONTRACT_MANAGER | ROLE_DIRECTOR,
    );
    let mut sess2 = setup_session(&ctx, char2, corp2, station_id, 0);
    let _sess3 = setup_session(&ctx, char3, corp3, station_id, 0);

    let personal_item = seed_hangar_item(&ctx, char1, station_id, 34, 400);
    let corp_item = seed_hangar_item(&ctx, char1, station_id, 35, 300);

    // 1. Personal public auction issued by char1 (`forCorp == false`) with bid by char2
    let personal_cid = create_contract_rpc(
        &ctx,
        &mut sess1,
        2,
        1,
        station_id,
        station_id,
        100_000.0,
        0.0,
        500_000.0,
        "Personal Public Auction",
        personal_item,
        400,
        false,
    )
    .await;

    ctx.dispatch_rpc(
        &mut sess2,
        "contractProxy",
        None,
        "PlaceBid",
        &[PyRep::Integer(personal_cid), PyRep::Real(150_000.0)],
        &empty,
        None,
    )
    .await
    .expect("PlaceBid personal auction");

    // 2. Corporate public auction issued by char1 (`forCorp == true`) with bid by char2
    let corp_cid = create_contract_rpc(
        &ctx,
        &mut sess1,
        2,
        1,
        station_id,
        station_id,
        200_000.0,
        0.0,
        900_000.0,
        "Corp Public Auction",
        corp_item,
        300,
        true,
    )
    .await;

    ctx.dispatch_rpc(
        &mut sess2,
        "contractProxy",
        None,
        "PlaceBid",
        &[PyRep::Integer(corp_cid), PyRep::Real(275_000.0)],
        &empty,
        None,
    )
    .await
    .expect("PlaceBid corp auction");

    let tok1 = issue_token_with_scopes(
        &ctx,
        char1,
        vec![
            "esi-contracts.read_character_contracts.v1",
            "esi-contracts.read_corporation_contracts.v1",
        ],
    );
    let tok3 = issue_token_with_scopes(&ctx, char3, vec!["esi-contracts.read_character_contracts.v1"]);

    // Unrelated character `char3` gets 403 on `/characters/{char3}/contracts/{cid}/{items,bids}` for BOTH public contracts
    for cid in [personal_cid, corp_cid] {
        assert_status(
            &router,
            format!("/v1/characters/{char3}/contracts/{cid}/items/"),
            Some(&tok3),
            403,
        );
        assert_status(
            &router,
            format!("/v1/characters/{char3}/contracts/{cid}/bids/"),
            Some(&tok3),
            403,
        );
    }

    // `for_corp == true` issuer `char1` gets 403 on `/characters/{char1}/contracts/{corp_cid}/{items,bids}`
    assert_status(
        &router,
        format!("/v1/characters/{char1}/contracts/{corp_cid}/items/"),
        Some(&tok1),
        403,
    );
    assert_status(
        &router,
        format!("/v1/characters/{char1}/contracts/{corp_cid}/bids/"),
        Some(&tok1),
        403,
    );

    // Public bids endpoint strips `bidder_id`, while personal issuer & corp routes preserve `bidder_id`
    let pub_bids = get_json(&router, &spec, format!("/v1/contracts/public/bids/{personal_cid}/"), None);
    assert_eq!(pub_bids[0]["amount"], 150_000.0);
    assert!(pub_bids[0].get("bidder_id").is_none(), "public bids must omit bidder_id");

    let issuer_bids = get_json(
        &router,
        &spec,
        format!("/v1/characters/{char1}/contracts/{personal_cid}/bids/"),
        Some(&tok1),
    );
    assert_eq!(issuer_bids[0]["bidder_id"], char2);

    let corp_bids = get_json(
        &router,
        &spec,
        format!("/v1/corporations/{corp1}/contracts/{corp_cid}/bids/"),
        Some(&tok1),
    );
    assert_eq!(corp_bids[0]["bidder_id"], char2);

    let _ = std::fs::remove_file(&db_path);
}

// parity: esi:GET /characters/{character_id}/contracts, esi:GET /contracts/public/{region_id}, esi:GET /contracts/public/bids/{contract_id}, esi:GET /contracts/public/items/{contract_id}, esi:GET /corporations/{corporation_id}/contracts
#[tokio::test]
async fn test_esi_p6_public_contract_204_on_accept_and_expiry_and_30d_retention() {
    let (db_path, ctx) = open_seeded_sqlite_ctx("lifecycle_retention");
    let router = EsiRouter::new(ctx.clone());
    let spec = OpenApiSpec::pinned();
    let empty = HashMap::new();

    let start_station = 60_003_760_i64;
    let dest_station = 60_003_757_i64;
    let region_id = 10_000_002_i64;
    let corp1 = 98_000_511_i64;
    let (char1, char2, corp2) = ctx.db.with_snapshot(|snap| {
        let mut chars: Vec<_> = snap.characters.values().collect();
        chars.sort_by_key(|c| c.character_id);
        (
            chars[0].character_id,
            chars[1].character_id,
            chars[1].corporation_id,
        )
    });
    let _: Result<(), ()> = ctx.db.try_with_mut_snapshot(|snap| {
        let mut c1 = CorporationRecord {
            corporation_id: corp1,
            name: "Retention Corp".into(),
            ticker: "RETC".into(),
            ceo_id: char1,
            master_wallet: IskCents::from_isk(500_000_000),
            ..Default::default()
        };
        c1.ensure_wallet_divisions();
        snap.corporations.insert(corp1, c1);
        Ok(())
    });

    let mut sess1 = setup_session(
        &ctx,
        char1,
        corp1,
        start_station,
        ROLE_CONTRACT_MANAGER | ROLE_DIRECTOR,
    );
    let mut sess2 = setup_session(&ctx, char2, corp2, start_station, 0);

    let exchange_item = seed_hangar_item(&ctx, char1, start_station, 34, 200);
    let expiring_item = seed_hangar_item(&ctx, char1, start_station, 35, 150);
    let courier_item = seed_hangar_item(&ctx, char1, start_station, 36, 50);
    let corp_exchange_item = seed_hangar_item(&ctx, char1, start_station, 37, 100);

    let accepted_cid = create_contract_rpc(
        &ctx,
        &mut sess1,
        1,
        1,
        start_station,
        start_station,
        50_000.0,
        0.0,
        0.0,
        "Accepted Exchange",
        exchange_item,
        200,
        false,
    )
    .await;
    let expiring_cid = create_contract_rpc(
        &ctx,
        &mut sess1,
        2,
        1,
        start_station,
        start_station,
        100_000.0,
        0.0,
        400_000.0,
        "Expiring Auction",
        expiring_item,
        150,
        false,
    )
    .await;
    let courier_cid = create_contract_rpc(
        &ctx,
        &mut sess1,
        3,
        28,
        start_station,
        dest_station,
        0.0,
        25_000.0,
        10_000.0,
        "Long InProgress Courier",
        courier_item,
        50,
        false,
    )
    .await;
    let corp_cid = create_contract_rpc(
        &ctx,
        &mut sess1,
        1,
        1,
        start_station,
        start_station,
        60_000.0,
        0.0,
        0.0,
        "Corp Exchange",
        corp_exchange_item,
        100,
        true,
    )
    .await;

    // While Outstanding & unexpired, public sub-routes return 200 OK
    assert_eq!(
        get_json(&router, &spec, format!("/v1/contracts/public/items/{accepted_cid}/"), None)
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        get_json(&router, &spec, format!("/v1/contracts/public/items/{expiring_cid}/"), None)
            .as_array()
            .unwrap()
            .len(),
        1
    );

    // Accept contracts A, C, and D via live `contractProxy.AcceptContract`
    for cid in [accepted_cid, courier_cid, corp_cid] {
        ctx.dispatch_rpc(
            &mut sess2,
            "contractProxy",
            None,
            "AcceptContract",
            &[PyRep::Integer(cid), PyRep::Bool(false)],
            &empty,
            None,
        )
        .await
        .expect("AcceptContract");
    }

    // Accepted contracts immediately return 204 No Content on `/contracts/public/{items,bids}/{cid}`
    assert_204_valid(&router, &spec, format!("/v1/contracts/public/items/{accepted_cid}/"));
    assert_204_valid(&router, &spec, format!("/v1/contracts/public/bids/{accepted_cid}/"));
    assert_204_valid(&router, &spec, format!("/v1/contracts/public/items/{courier_cid}/"));

    // Advance clock by 2 days -> `expiring_cid` expires; public region omits it and public items/bids return 204
    ctx.db.clock().advance_secs(2 * 86_400);
    let pub_after_2d = get_json(&router, &spec, format!("/v1/contracts/public/{region_id}/"), None);
    assert!(
        !pub_after_2d
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["contract_id"] == expiring_cid || c["contract_id"] == accepted_cid)
    );
    assert_204_valid(&router, &spec, format!("/v1/contracts/public/items/{expiring_cid}/"));
    assert_204_valid(&router, &spec, format!("/v1/contracts/public/bids/{expiring_cid}/"));

    // Within 30 days (`+2d`), finished/expired contracts are still retained in character & corporation lists
    let tok1 = issue_token_with_scopes(
        &ctx,
        char1,
        vec![
            "esi-contracts.read_character_contracts.v1",
            "esi-contracts.read_corporation_contracts.v1",
        ],
    );
    let char_2d = get_json(&router, &spec, format!("/v1/characters/{char1}/contracts/"), Some(&tok1));
    let char_2d_ids: Vec<i64> = char_2d
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|c| c["contract_id"].as_i64())
        .collect();
    assert!(char_2d_ids.contains(&accepted_cid));
    assert!(char_2d_ids.contains(&expiring_cid));
    assert!(char_2d_ids.contains(&courier_cid));

    let corp_2d = get_json(&router, &spec, format!("/v1/corporations/{corp1}/contracts/"), Some(&tok1));
    assert!(corp_2d.as_array().unwrap().iter().any(|c| c["contract_id"] == corp_cid));

    // Advance clock past 30 days (`+29d` -> total `+31d`): non-`in_progress` contracts older than 30 days are pruned,
    // while `in_progress` courier contract C remains visible.
    ctx.db.clock().advance_secs(29 * 86_400);
    let tok1_31d = issue_token_with_scopes(
        &ctx,
        char1,
        vec![
            "esi-contracts.read_character_contracts.v1",
            "esi-contracts.read_corporation_contracts.v1",
        ],
    );
    let char_31d = get_json(
        &router,
        &spec,
        format!("/v1/characters/{char1}/contracts/"),
        Some(&tok1_31d),
    );
    let char_31d_ids: Vec<i64> = char_31d
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|c| c["contract_id"].as_i64())
        .collect();
    assert!(
        !char_31d_ids.contains(&accepted_cid) && !char_31d_ids.contains(&expiring_cid),
        "finished/expired contracts > 30 days old must be excluded from character contracts"
    );
    assert!(
        char_31d_ids.contains(&courier_cid),
        "in_progress courier contract must remain visible past 30 days"
    );

    let corp_31d = get_json(
        &router,
        &spec,
        format!("/v1/corporations/{corp1}/contracts/"),
        Some(&tok1_31d),
    );
    assert!(
        !corp_31d.as_array().unwrap().iter().any(|c| c["contract_id"] == corp_cid),
        "completed corporation contract > 30 days old must be excluded from corporation contracts"
    );

    // Verify SQLite persistence across reopen
    drop(router);
    drop(ctx);
    let repo2 = Arc::new(WorldRepository::open_sqlite(&db_path).expect("reopen sqlite"));
    let ctx2 = Arc::new(ServerContext::with_repository(repo2));
    crate::common::pin_clock(&ctx2);
    ctx2.db.clock().advance_secs(31 * 86_400);
    let router2 = EsiRouter::new(ctx2);
    assert_204_valid(&router2, &spec, format!("/v1/contracts/public/items/{accepted_cid}/"));
    let tok1_reopen = issue_token_with_scopes(
        &router2.ctx,
        char1,
        vec!["esi-contracts.read_character_contracts.v1"],
    );
    let char_reopen = get_json(
        &router2,
        &spec,
        format!("/v1/characters/{char1}/contracts/"),
        Some(&tok1_reopen),
    );
    let reopen_ids: Vec<i64> = char_reopen
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|c| c["contract_id"].as_i64())
        .collect();
    assert!(reopen_ids.contains(&courier_cid));
    assert!(!reopen_ids.contains(&accepted_cid));
    let _ = std::fs::remove_file(&db_path);
}
