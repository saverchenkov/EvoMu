//! `ESI-6` live HTTP + RPC + SQLite persistence integration tests for Contracts and Industry routes.

#![forbid(unsafe_code)]

use crate::esi_market_industry_pi::{assert_status, get_json, issue_token_with_scopes, validate_and_json};
use eve_db::{
    corp_roles::{ROLE_CONTRACT_MANAGER, ROLE_DIRECTOR, ROLE_FACTORY_MANAGER},
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
    std::env::temp_dir().join(format!("evomu_p6_esi1_{tag}_{nanos}.sqlite"))
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
    (db_path, ctx)
}

fn setup_session(ctx: &ServerContext, char_id: i64, corp_id: i64, station_id: i64, roles: u64) -> SessionState {
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
        }
        if let Some(corp) = snap.corporations.get_mut(&corp_id) {
            corp.members.insert(char_id);
            corp.member_roles.insert(char_id, roles);
        }
        Ok(())
    });
    sess
}

fn seed_hangar_item(ctx: &ServerContext, owner_id: i64, station_id: i64, type_id: u32, qty: i32) -> i64 {
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

// parity: esi:GET /characters/{character_id}/contracts, esi:GET /characters/{character_id}/contracts/{contract_id}/bids, esi:GET /characters/{character_id}/contracts/{contract_id}/items, esi:GET /contracts/public/{region_id}, esi:GET /contracts/public/bids/{contract_id}, esi:GET /contracts/public/items/{contract_id}
#[tokio::test]
async fn test_esi_p6_char_and_public_contracts_live_rpc_and_sqlite_persistence() {
    let (db_path, ctx) = open_seeded_sqlite_ctx("char_public_contracts");
    let router = EsiRouter::new(ctx.clone());
    let spec = OpenApiSpec::pinned();
    let empty = HashMap::new();

    let station_id = 60_003_760_i64;
    let region_id = 10_000_002_i64;
    let (char1, corp1, char2, corp2) = ctx.db.with_snapshot(|snap| {
        let mut chars: Vec<_> = snap.characters.values().collect();
        chars.sort_by_key(|c| c.character_id);
        (
            chars[0].character_id,
            chars[0].corporation_id,
            chars[1].character_id,
            chars[1].corporation_id,
        )
    });

    let mut sess1 = setup_session(&ctx, char1, corp1, station_id, 0);
    let mut sess2 = setup_session(&ctx, char2, corp2, station_id, 0);
    let item_id = seed_hangar_item(&ctx, char1, station_id, 34, 500);

    let c_res = ctx
        .dispatch_rpc(
            &mut sess1,
            "contractProxy",
            None,
            "CreateContract",
            &[
                PyRep::Integer(2), // Auction
                PyRep::Integer(0), // Public
                PyRep::Integer(0),
                PyRep::Integer(1440),
                PyRep::Integer(1),
                PyRep::Integer(station_id),
                PyRep::Integer(station_id),
                PyRep::Real(100_000.0),
                PyRep::Real(0.0),
                PyRep::Real(500_000.0),
                PyRep::String("P6 Tritanium Auction".into()),
                PyRep::String("Live RPC auction".into()),
                PyRep::List(vec![PyRep::Tuple(vec![
                    PyRep::Integer(item_id),
                    PyRep::Integer(500),
                ])]),
            ],
            &empty,
            None,
        )
        .await
        .expect("CreateContract");
    let PyRep::KeyVal(kv) = c_res else {
        panic!("expected KeyVal from CreateContract");
    };
    let cid = kv.get("contractID").unwrap().as_i64().unwrap();

    ctx.dispatch_rpc(
        &mut sess2,
        "contractProxy",
        None,
        "PlaceBid",
        &[PyRep::Integer(cid), PyRep::Real(125_000.0)],
        &empty,
        None,
    )
    .await
    .expect("PlaceBid");

    let tok1 = issue_token_with_scopes(&ctx, char1, vec!["esi-contracts.read_character_contracts.v1"]);
    let tok2 = issue_token_with_scopes(&ctx, char2, vec!["esi-contracts.read_character_contracts.v1"]);
    let bad_scope = issue_token_with_scopes(&ctx, char1, vec!["esi-wallet.read_character_wallet.v1"]);

    // Auth / 401 / 403 / 404 checks
    assert_status(&router, format!("/v1/characters/{char1}/contracts/"), None, 401);
    assert_status(&router, format!("/v1/characters/{char1}/contracts/"), Some(&bad_scope), 403);
    assert_status(&router, format!("/v1/characters/{char1}/contracts/"), Some(&tok2), 403);
    assert_status(&router, "/v1/characters/999999999/contracts/", Some(&tok1), 403);
    assert_status(&router, format!("/v1/characters/{char1}/contracts/999999999/items/"), Some(&tok1), 404);
    assert_status(&router, format!("/v1/characters/{char1}/contracts/999999999/bids/"), Some(&tok1), 404);
    assert_status(&router, "/v1/contracts/public/999999999/", None, 404);

    // Character contracts, items, bids
    let char_contracts = get_json(&router, &spec, format!("/v1/characters/{char1}/contracts/"), Some(&tok1));
    let found = char_contracts
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["contract_id"] == cid)
        .expect("contract in char list");
    assert_eq!(found["type"], "auction");
    assert_eq!(found["status"], "outstanding");
    assert_eq!(found["title"], "P6 Tritanium Auction");

    let items_req = EsiHttpRequest::new("GET", format!("/v1/characters/{char1}/contracts/{cid}/items/"))
        .with_bearer_token(&tok1);
    let items_resp = router.handle_request(&items_req);
    assert_eq!(items_resp.status_code, 200);
    assert!(items_resp.header("X-Pages").is_none());
    let char_items = validate_and_json(&router, &spec, &items_req, 200);
    assert_eq!(char_items.as_array().unwrap().len(), 1);
    assert_eq!(char_items[0]["type_id"], 34);
    assert_eq!(char_items[0]["quantity"], 500);

    let char_bids = get_json(&router, &spec, format!("/v1/characters/{char1}/contracts/{cid}/bids/"), Some(&tok1));
    assert_eq!(char_bids.as_array().unwrap().len(), 1);
    assert_eq!(char_bids[0]["amount"], 125_000.0);
    assert_eq!(char_bids[0]["bidder_id"], char2);

    // Public contracts, items, bids
    let pub_contracts = get_json(&router, &spec, format!("/v1/contracts/public/{region_id}/"), None);
    assert!(pub_contracts.as_array().unwrap().iter().any(|c| c["contract_id"] == cid));

    let pub_items = get_json(&router, &spec, format!("/v1/contracts/public/items/{cid}/"), None);
    assert_eq!(pub_items.as_array().unwrap().len(), 1);
    assert_eq!(pub_items[0]["type_id"], 34);

    let pub_bids = get_json(&router, &spec, format!("/v1/contracts/public/bids/{cid}/"), None);
    assert_eq!(pub_bids.as_array().unwrap().len(), 1);
    assert_eq!(pub_bids[0]["amount"], 125_000.0);

    // SQLite persistence check across repository reopen
    drop(router);
    drop(ctx);
    let repo2 = Arc::new(WorldRepository::open_sqlite(&db_path).expect("reopen sqlite"));
    let ctx2 = Arc::new(ServerContext::with_repository(repo2));
    let router2 = EsiRouter::new(ctx2);
    let pub_bids2 = get_json(&router2, &spec, format!("/v1/contracts/public/bids/{cid}/"), None);
    assert_eq!(pub_bids2.as_array().unwrap().len(), 1);
    assert_eq!(pub_bids2[0]["amount"], 125_000.0);
    let _ = std::fs::remove_file(&db_path);
}

// parity: esi:GET /corporations/{corporation_id}/contracts, esi:GET /corporations/{corporation_id}/contracts/{contract_id}/bids, esi:GET /corporations/{corporation_id}/contracts/{contract_id}/items
#[tokio::test]
async fn test_esi_p6_corp_contracts_live_rpc_auth_and_sqlite_persistence() {
    let (db_path, ctx) = open_seeded_sqlite_ctx("corp_contracts");
    let router = EsiRouter::new(ctx.clone());
    let spec = OpenApiSpec::pinned();
    let empty = HashMap::new();

    let station_id = 60_003_760_i64;
    let corp1 = 98_000_001_i64;
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
        let mut corp = CorporationRecord {
            corporation_id: corp1,
            name: "P6 Corp".to_string(),
            ticker: "P6C".to_string(),
            ceo_id: char1,
            master_wallet: IskCents::from_isk(500_000_000),
            ..Default::default()
        };
        corp.ensure_wallet_divisions();
        snap.corporations.insert(corp1, corp);
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
    let item_id = seed_hangar_item(&ctx, char1, station_id, 35, 250);

    let mut kwargs = HashMap::new();
    kwargs.insert("forCorp".to_string(), PyRep::Bool(true));
    let c_res = ctx
        .dispatch_rpc(
            &mut sess1,
            "contractProxy",
            None,
            "CreateContract",
            &[
                PyRep::Integer(2), // Auction
                PyRep::Integer(0), // Public
                PyRep::Integer(0),
                PyRep::Integer(1440),
                PyRep::Integer(1),
                PyRep::Integer(station_id),
                PyRep::Integer(station_id),
                PyRep::Real(200_000.0),
                PyRep::Real(0.0),
                PyRep::Real(800_000.0),
                PyRep::String("P6 Corp Pyerite Auction".into()),
                PyRep::String("Corp auction".into()),
                PyRep::List(vec![PyRep::Tuple(vec![
                    PyRep::Integer(item_id),
                    PyRep::Integer(250),
                ])]),
            ],
            &kwargs,
            None,
        )
        .await
        .expect("CreateContract forCorp");
    let PyRep::KeyVal(kv) = c_res else {
        panic!("expected KeyVal");
    };
    let cid = kv.get("contractID").unwrap().as_i64().unwrap();

    ctx.dispatch_rpc(
        &mut sess2,
        "contractProxy",
        None,
        "PlaceBid",
        &[PyRep::Integer(cid), PyRep::Real(250_000.0)],
        &empty,
        None,
    )
    .await
    .expect("PlaceBid on corp auction");

    let tok_corp = issue_token_with_scopes(&ctx, char1, vec!["esi-contracts.read_corporation_contracts.v1"]);
    let tok_other = issue_token_with_scopes(&ctx, char2, vec!["esi-contracts.read_corporation_contracts.v1"]);

    assert_status(&router, format!("/v1/corporations/{corp1}/contracts/"), None, 401);
    assert_status(&router, format!("/v1/corporations/{corp1}/contracts/"), Some(&tok_other), 403);
    assert_status(&router, format!("/v1/corporations/{corp1}/contracts/999999999/items/"), Some(&tok_corp), 404);
    assert_status(&router, format!("/v1/corporations/{corp1}/contracts/999999999/bids/"), Some(&tok_corp), 404);

    let corp_contracts = get_json(&router, &spec, format!("/v1/corporations/{corp1}/contracts/"), Some(&tok_corp));
    let found = corp_contracts
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["contract_id"] == cid)
        .expect("corp contract in list");
    assert_eq!(found["for_corporation"], true);
    assert_eq!(found["issuer_corporation_id"], corp1);

    let corp_items = get_json(
        &router,
        &spec,
        format!("/v1/corporations/{corp1}/contracts/{cid}/items/"),
        Some(&tok_corp),
    );
    assert_eq!(corp_items.as_array().unwrap().len(), 1);
    assert_eq!(corp_items[0]["type_id"], 35);
    assert_eq!(corp_items[0]["quantity"], 250);

    let corp_bids = get_json(
        &router,
        &spec,
        format!("/v1/corporations/{corp1}/contracts/{cid}/bids/"),
        Some(&tok_corp),
    );
    assert_eq!(corp_bids.as_array().unwrap().len(), 1);
    assert_eq!(corp_bids[0]["amount"], 250_000.0);

    // SQLite persistence check
    drop(router);
    drop(ctx);
    let repo2 = Arc::new(WorldRepository::open_sqlite(&db_path).expect("reopen sqlite"));
    let ctx2 = Arc::new(ServerContext::with_repository(repo2));
    let router2 = EsiRouter::new(ctx2);
    let tok_corp2 = issue_token_with_scopes(&router2.ctx, char1, vec!["esi-contracts.read_corporation_contracts.v1"]);
    let corp_bids2 = get_json(
        &router2,
        &spec,
        format!("/v1/corporations/{corp1}/contracts/{cid}/bids/"),
        Some(&tok_corp2),
    );
    assert_eq!(corp_bids2.as_array().unwrap().len(), 1);
    let _ = std::fs::remove_file(&db_path);
}

// parity: esi:GET /characters/{character_id}/industry/jobs, esi:GET /corporations/{corporation_id}/industry/jobs, esi:GET /industry/facilities
#[tokio::test]
async fn test_esi_p6_industry_jobs_and_facilities_live_rpc_and_sqlite_persistence() {
    let (db_path, ctx) = open_seeded_sqlite_ctx("industry_jobs_facilities");
    let router = EsiRouter::new(ctx.clone());
    let spec = OpenApiSpec::pinned();
    let empty = HashMap::new();

    let station_id = 60_003_760_i64;
    let corp1 = 98_000_001_i64;
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
        let mut corp = CorporationRecord {
            corporation_id: corp1,
            name: "P6 Corp".to_string(),
            ticker: "P6C".to_string(),
            ceo_id: char1,
            master_wallet: IskCents::from_isk(500_000_000),
            ..Default::default()
        };
        corp.ensure_wallet_divisions();
        snap.corporations.insert(corp1, corp);
        Ok(())
    });

    let mut sess1 = setup_session(
        &ctx,
        char1,
        corp1,
        station_id,
        ROLE_FACTORY_MANAGER | ROLE_DIRECTOR,
    );
    let _sess2 = setup_session(&ctx, char2, corp2, station_id, 0);

    // Install character manufacturing job via industryManager.InstallJob
    let job_res = ctx
        .dispatch_rpc(
            &mut sess1,
            "industryManager",
            None,
            "InstallJob",
            &[PyRep::Integer(691), PyRep::Integer(1), PyRep::Integer(1)],
            &empty,
            None,
        )
        .await
        .expect("InstallJob");
    let PyRep::KeyVal(job_kv) = job_res else {
        panic!("expected KeyVal from InstallJob");
    };
    let job_id = job_kv.get("jobID").unwrap().as_i64().unwrap();

    // Public /industry/facilities endpoint
    let facilities = get_json(&router, &spec, "/v1/industry/facilities/", None);
    let fac_arr = facilities.as_array().unwrap();
    assert!(!fac_arr.is_empty(), "SDE NPC industry facilities must be listed");
    if let Some(jita_fac) = fac_arr.iter().find(|f| f["facility_id"] == station_id) {
        assert_eq!(jita_fac["tax"], 0.0025);
    }

    let tok_char = issue_token_with_scopes(&ctx, char1, vec!["esi-industry.read_character_jobs.v1"]);
    let tok_other = issue_token_with_scopes(&ctx, char2, vec!["esi-industry.read_character_jobs.v1"]);

    assert_status(&router, format!("/v1/characters/{char1}/industry/jobs/"), None, 401);
    assert_status(&router, format!("/v1/characters/{char1}/industry/jobs/"), Some(&tok_other), 403);
    assert_status(&router, format!("/v1/corporations/{corp1}/industry/jobs/"), None, 401);
    assert_status(&router, format!("/v1/corporations/{corp1}/industry/jobs/"), Some(&tok_other), 403);

    let char_jobs = get_json(&router, &spec, format!("/v1/characters/{char1}/industry/jobs/"), Some(&tok_char));
    let j = char_jobs
        .as_array()
        .unwrap()
        .iter()
        .find(|j| j["job_id"] == job_id)
        .expect("installed job in char jobs");
    assert_eq!(j["status"], "active");
    assert_eq!(j["facility_id"], station_id);

    // Complete job and verify include_completed=false vs true
    ctx.advance_virtual_clock(6_000);
    let tok_char = issue_token_with_scopes(&ctx, char1, vec!["esi-industry.read_character_jobs.v1"]);
    let tok_corp = issue_token_with_scopes(&ctx, char1, vec!["esi-industry.read_corporation_jobs.v1"]);
    ctx.dispatch_rpc(
        &mut sess1,
        "industryManager",
        None,
        "CompleteJob",
        &[PyRep::Integer(job_id)],
        &empty,
        None,
    )
    .await
    .expect("CompleteJob");

    let active_only = get_json(
        &router,
        &spec,
        format!("/v1/characters/{char1}/industry/jobs/?include_completed=false"),
        Some(&tok_char),
    );
    assert!(!active_only.as_array().unwrap().iter().any(|j| j["job_id"] == job_id));

    let all_jobs = get_json(
        &router,
        &spec,
        format!("/v1/characters/{char1}/industry/jobs/?include_completed=true"),
        Some(&tok_char),
    );
    let completed = all_jobs
        .as_array()
        .unwrap()
        .iter()
        .find(|j| j["job_id"] == job_id)
        .expect("completed job");
    assert_eq!(completed["status"], "delivered");
    assert_eq!(completed["completed_character_id"], char1);

    // Install corporation manufacturing job via industryManager.InstallJob (useCorp = true)
    let mut corp_kwargs = HashMap::new();
    corp_kwargs.insert("useCorp".to_string(), PyRep::Bool(true));
    let corp_job_res = ctx
        .dispatch_rpc(
            &mut sess1,
            "industryManager",
            None,
            "InstallJob",
            &[PyRep::Integer(691), PyRep::Integer(1), PyRep::Integer(1)],
            &corp_kwargs,
            None,
        )
        .await
        .expect("InstallJob useCorp");
    let PyRep::KeyVal(corp_job_kv) = corp_job_res else {
        panic!("expected KeyVal from corp InstallJob");
    };
    let corp_job_id = corp_job_kv.get("jobID").unwrap().as_i64().unwrap();

    let corp_jobs = get_json(&router, &spec, format!("/v1/corporations/{corp1}/industry/jobs/"), Some(&tok_corp));
    assert!(corp_jobs.as_array().unwrap().iter().any(|j| j["job_id"] == corp_job_id));

    // SQLite persistence check
    drop(router);
    drop(ctx);
    let repo2 = Arc::new(WorldRepository::open_sqlite(&db_path).expect("reopen sqlite"));
    let ctx2 = Arc::new(ServerContext::with_repository(repo2));
    let router2 = EsiRouter::new(ctx2);
    let tok_char2 = issue_token_with_scopes(&router2.ctx, char1, vec!["esi-industry.read_character_jobs.v1"]);
    let persisted = get_json(
        &router2,
        &spec,
        format!("/v1/characters/{char1}/industry/jobs/?include_completed=true"),
        Some(&tok_char2),
    );
    assert!(persisted.as_array().unwrap().iter().any(|j| j["job_id"] == job_id));
    let _ = std::fs::remove_file(&db_path);
}
