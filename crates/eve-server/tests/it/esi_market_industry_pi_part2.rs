//! Remediation integration tests for:
//! - ESI Market Groups localized SDE descriptions (`esi:GET /markets/groups/{market_group_id}`, `feat:esi.market.groups`)
//! - ESI Structure Market Orders Hub & ACL enforcement (`esi:GET /markets/structures/{structure_id}`, `feat:esi.market.structure-orders`)
//! - ESI Corporation endpoint-specific OpenAPI role gates & route registration across all 6 corp endpoints

#![forbid(unsafe_code)]

use eve_db::{
    corp_roles, CorporationRecord, CustomsOfficeRecord, InventoryFlag, InventoryItemRecord,
    IskCents, MarketOrderMeta, MarketOrderRecord, MoonExtractionRecord, ObserverMiningRecord,
    UpwellStructureRecord,
};
use eve_server::esi::{EsiHttpRequest, EsiRouter, OpenApiSpec};
use eve_server::router::ServerContext;
use eve_server::services::station_structure::structure_upwell_ops::{
    load_ext_state, save_ext_state, StructureAccessProfile,
};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

const PKCE_VERIFIER: &str = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
const PKCE_CHALLENGE: &str = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM";

fn issue_token_with_scopes(ctx: &ServerContext, character_id: i64, scopes: Vec<&str>) -> String {
    let now = ctx.now_unix();
    let scope_vec: Vec<String> = scopes.into_iter().map(str::to_string).collect();
    let code = ctx
        .db
        .authorize_pkce(
            "demo",
            "demo-secret",
            character_id,
            "evomu-esi-remed-test",
            "https://localhost/callback",
            scope_vec,
            PKCE_CHALLENGE,
            "S256",
            now,
        )
        .expect("authorize_pkce");
    let bundle = ctx
        .db
        .exchange_pkce_code(&code, "evomu-esi-remed-test", PKCE_VERIFIER, now)
        .expect("exchange_pkce_code");
    bundle.access_token
}

fn get_validated_json(
    router: &EsiRouter,
    spec: &OpenApiSpec,
    target: &str,
    token: Option<&str>,
) -> Value {
    let mut req = EsiHttpRequest::new("GET", target);
    if let Some(t) = token {
        req = req.with_bearer_token(t);
    }
    let resp = router.handle_request(&req);
    let raw_path = req.raw_target.split('?').next().unwrap_or(&req.raw_target);
    assert_eq!(
        resp.status_code,
        200,
        "Expected 200 for {target}, got {} ({})",
        resp.status_code,
        String::from_utf8_lossy(&resp.body)
    );
    spec.validate_response("GET", raw_path, 200, &resp.headers, &resp.body)
        .unwrap_or_else(|e| panic!("OpenAPI validation failed for {target}: {e}"));
    resp.json().expect("valid JSON")
}

fn assert_status(router: &EsiRouter, target: &str, token: Option<&str>, expected: u16) {
    let mut req = EsiHttpRequest::new("GET", target);
    if let Some(t) = token {
        req = req.with_bearer_token(t);
    }
    let resp = router.handle_request(&req);
    assert_eq!(
        resp.status_code,
        expected,
        "Expected {expected} for {target}, got {} ({})",
        resp.status_code,
        String::from_utf8_lossy(&resp.body)
    );
}

fn make_structure(
    structure_id: i64,
    type_id: u32,
    owner_corp_id: i64,
    solar_system_id: u32,
    name: &str,
) -> UpwellStructureRecord {
    UpwellStructureRecord {
        structure_id,
        type_id,
        owner_corp_id,
        solar_system_id,
        name: name.to_string(),
        position: [1000.0, 2000.0, 3000.0],
        state: "ShieldVulnerable".to_string(),
        quantum_core_installed: true,
        fuel_blocks: 500,
        shield_hp: 10000.0,
        armor_hp: 10000.0,
        hull_hp: 10000.0,
        reinforced_until_unix: 0,
        last_damage_tick: 0,
        last_damage_unix: 0,
        self_destruct_at_unix: None,
    }
}

#[test]
fn test_esi_market_groups_sde_descriptions_and_structure_orders_acl_hub() {
    // parity: esi:GET /markets/groups/{market_group_id}, feat:esi.market.groups, esi:GET /markets/structures/{structure_id}, feat:esi.market.structure-orders
    eve_testkit::attest(&[
        "esi:GET /markets/groups/{market_group_id}",
        "feat:esi.market.groups",
        "esi:GET /markets/structures/{structure_id}",
        "feat:esi.market.structure-orders",
    ]);

    let ctx = Arc::new(ServerContext::new());
    crate::common::pin_clock(&ctx);
    let router = EsiRouter::new(Arc::clone(&ctx));
    let spec = OpenApiSpec::pinned();
    let now = ctx.now_unix();

    // 1. Verify market group descriptions come from V24 marketgroups.fsdbinary + localization pickle (not name)
    for mg_id in [4u32, 9u32] {
        let expected_desc = ctx
            .sde
            .market_group_description(mg_id)
            .expect("SDE localized market group description");
        assert!(!expected_desc.is_empty());
        let detail = get_validated_json(
            &router,
            &spec,
            &format!("/v1/markets/groups/{mg_id}/"),
            None,
        );
        assert_eq!(detail["description"], json!(expected_desc));
        assert_ne!(detail["description"], detail["name"]);
    }

    // 2. Verify structure market orders require online Standup Market Hub I (35892) AND docking/market ACL access
    let char_id = 90_000_001_i64;
    let corp_id = 98_000_711_i64;
    let fortizar_hub_id = 1_000_000_711_001_i64;
    let bare_astrahus_id = 1_000_000_711_002_i64;
    let offline_hub_fortizar_id = 1_000_000_711_003_i64;
    let acl_blocked_fortizar_id = 1_000_000_711_004_i64;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.user_id = 1;
            ch.corporation_id = corp_id;
        }
        snap.corporations.insert(
            corp_id,
            CorporationRecord {
                corporation_id: corp_id,
                name: "Market Hub Test Corp".into(),
                ticker: "MHTC".into(),
                ceo_id: char_id,
                members: HashSet::from([char_id]),
                ..Default::default()
            },
        );

        // (a) Fortizar with online Standup Market Hub I (35892)
        snap.structures.insert(
            fortizar_hub_id,
            make_structure(fortizar_hub_id, 35833, corp_id, 30_000_142, "Hub Fortizar"),
        );
        snap.items.insert(
            950_001,
            InventoryItemRecord {
                item_id: 950_001,
                type_id: 35_892,
                owner_id: corp_id,
                location_id: fortizar_hub_id,
                flag: InventoryFlag::ServiceSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );

        // (b) Bare Astrahus (35832) without Market Hub
        snap.structures.insert(
            bare_astrahus_id,
            make_structure(bare_astrahus_id, 35832, corp_id, 30_000_142, "Bare Astrahus"),
        );

        // (c) Fortizar with offline Standup Market Hub I
        snap.structures.insert(
            offline_hub_fortizar_id,
            make_structure(offline_hub_fortizar_id, 35833, corp_id, 30_000_142, "Offline Hub Fortizar"),
        );
        snap.items.insert(
            950_002,
            InventoryItemRecord {
                item_id: 950_002,
                type_id: 35_892,
                owner_id: corp_id,
                location_id: offline_hub_fortizar_id,
                flag: InventoryFlag::ServiceSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.module_online_states.insert(950_002, false);

        // (d) Fortizar with online Market Hub but docking ACL denied for caller
        snap.structures.insert(
            acl_blocked_fortizar_id,
            make_structure(acl_blocked_fortizar_id, 35833, corp_id, 30_000_142, "ACL Blocked Fortizar"),
        );
        snap.items.insert(
            950_003,
            InventoryItemRecord {
                item_id: 950_003,
                type_id: 35_892,
                owner_id: corp_id,
                location_id: acl_blocked_fortizar_id,
                flag: InventoryFlag::ServiceSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        let mut ext = load_ext_state(snap);
        ext.access_profiles.insert(
            acl_blocked_fortizar_id,
            StructureAccessProfile {
                profile_id: 1,
                owner_corp_id: corp_id,
                blocked_characters: std::collections::HashSet::from([char_id]),
                ..Default::default()
            },
        );
        save_ext_state(snap, &ext);

        for (idx, st_id) in [
            fortizar_hub_id,
            bare_astrahus_id,
            offline_hub_fortizar_id,
            acl_blocked_fortizar_id,
        ]
        .into_iter()
        .enumerate()
        {
            let oid = 77_100 + idx as i64;
            snap.market_orders.insert(
                oid,
                MarketOrderRecord {
                    order_id: oid,
                    type_id: 34,
                    owner_id: char_id,
                    station_id: st_id,
                    region_id: 10_000_002,
                    price: IskCents::from_isk(8),
                    vol_entered: 1_000,
                    vol_remaining: 1_000,
                    min_volume: 1,
                    is_bid: false,
                    is_npc: false,
                    escrow: IskCents::ZERO,
                },
            );
            snap.market_order_meta.insert(
                oid,
                MarketOrderMeta {
                    issued_unix: now,
                    duration_days: 30,
                    range: "station".into(),
                    issued_by: Some(char_id),
                    wallet_division: Some(1000),
                    is_corporation: false,
                    state: None,
                },
            );
        }
    });

    let token = issue_token_with_scopes(&ctx, char_id, vec!["esi-markets.structure_markets.v1"]);
    let ok_orders = get_validated_json(
        &router,
        &spec,
        &format!("/v1/markets/structures/{fortizar_hub_id}/"),
        Some(&token),
    );
    assert_eq!(ok_orders.as_array().unwrap().len(), 1);
    assert_eq!(ok_orders[0]["order_id"], json!(77_100));

    assert_status(
        &router,
        &format!("/v1/markets/structures/{bare_astrahus_id}/"),
        Some(&token),
        403,
    );
    assert_status(
        &router,
        &format!("/v1/markets/structures/{offline_hub_fortizar_id}/"),
        Some(&token),
        403,
    );
    assert_status(
        &router,
        &format!("/v1/markets/structures/{acl_blocked_fortizar_id}/"),
        Some(&token),
        403,
    );
}

#[test]
fn test_esi_corp_endpoint_specific_role_gates_and_routes() {
    // parity: esi:GET /corporations/{corporation_id}/orders, esi:GET /corporations/{corporation_id}/orders/history, feat:esi.market.corp-orders, esi:GET /corporation/{corporation_id}/mining/extractions, feat:esi.industry.corp-extractions, esi:GET /corporation/{corporation_id}/mining/observers, esi:GET /corporation/{corporation_id}/mining/observers/{observer_id}, feat:esi.industry.corp-observers, esi:GET /corporations/{corporation_id}/customs_offices, feat:esi.planetary-interaction.customs
    eve_testkit::attest(&[
        "esi:GET /corporations/{corporation_id}/orders",
        "esi:GET /corporations/{corporation_id}/orders/history",
        "feat:esi.market.corp-orders",
        "esi:GET /corporation/{corporation_id}/mining/extractions",
        "feat:esi.industry.corp-extractions",
        "esi:GET /corporation/{corporation_id}/mining/observers",
        "esi:GET /corporation/{corporation_id}/mining/observers/{observer_id}",
        "feat:esi.industry.corp-observers",
        "esi:GET /corporations/{corporation_id}/customs_offices",
        "feat:esi.planetary-interaction.customs",
    ]);

    let ctx = Arc::new(ServerContext::new());
    crate::common::pin_clock(&ctx);
    let router = EsiRouter::new(Arc::clone(&ctx));
    let spec = OpenApiSpec::pinned();
    let now = ctx.now_unix();

    let corp_id = 98_000_722_i64;
    let ceo_id = 90_000_001_i64;
    let no_role_id = 90_000_721_i64;
    let sec_officer_id = 90_000_722_i64;
    let accountant_id = 90_000_723_i64;
    let jr_accountant_id = 90_000_724_i64;
    let trader_id = 90_000_725_i64;
    let station_mgr_id = 90_000_726_i64;
    let observer_id = 1_000_000_722_001_i64;

    ctx.db.with_mut_snapshot(|snap| {
        let base_char = snap.characters[&ceo_id].clone();
        for cid in [
            ceo_id,
            no_role_id,
            sec_officer_id,
            accountant_id,
            jr_accountant_id,
            trader_id,
            station_mgr_id,
        ] {
            let mut ch = base_char.clone();
            ch.character_id = cid;
            ch.user_id = 1;
            ch.corporation_id = corp_id;
            snap.characters.insert(cid, ch);
        }
        snap.corporations.insert(
            corp_id,
            CorporationRecord {
                corporation_id: corp_id,
                name: "Role Gate Remediation Corp".into(),
                ticker: "RGRC".into(),
                ceo_id,
                members: HashSet::from([
                    ceo_id,
                    no_role_id,
                    sec_officer_id,
                    accountant_id,
                    jr_accountant_id,
                    trader_id,
                    station_mgr_id,
                ]),
                member_roles: HashMap::from([
                    (no_role_id, 0),
                    (sec_officer_id, corp_roles::ROLE_SECURITY_OFFICER),
                    (accountant_id, corp_roles::ROLE_ACCOUNTANT),
                    (jr_accountant_id, corp_roles::ROLE_JUNIOR_ACCOUNTANT),
                    (trader_id, corp_roles::ROLE_TRADER),
                    (station_mgr_id, corp_roles::ROLE_STATION_MANAGER),
                ]),
                ..Default::default()
            },
        );

        // Seed active & historical corp orders
        snap.market_orders.insert(
            88_101,
            MarketOrderRecord {
                order_id: 88_101,
                type_id: 34,
                owner_id: corp_id,
                station_id: 60_003_760,
                region_id: 10_000_002,
                price: IskCents::from_isk(9),
                vol_entered: 500,
                vol_remaining: 500,
                min_volume: 1,
                is_bid: false,
                is_npc: false,
                escrow: IskCents::ZERO,
            },
        );
        snap.market_order_meta.insert(
            88_101,
            MarketOrderMeta {
                issued_unix: now,
                duration_days: 30,
                range: "station".into(),
                issued_by: Some(trader_id),
                wallet_division: Some(1000),
                is_corporation: true,
                state: None,
            },
        );
        snap.market_orders.insert(
            88_102,
            MarketOrderRecord {
                order_id: 88_102,
                type_id: 34,
                owner_id: corp_id,
                station_id: 60_003_760,
                region_id: 10_000_002,
                price: IskCents::from_isk(9),
                vol_entered: 500,
                vol_remaining: 0,
                min_volume: 1,
                is_bid: false,
                is_npc: false,
                escrow: IskCents::ZERO,
            },
        );
        snap.market_order_meta.insert(
            88_102,
            MarketOrderMeta {
                issued_unix: now.saturating_sub(3600),
                duration_days: 30,
                range: "station".into(),
                issued_by: Some(trader_id),
                wallet_division: Some(1000),
                is_corporation: true,
                state: Some("expired".into()),
            },
        );

        // Seed moon extraction & mining observer
        snap.structures.insert(
            observer_id,
            make_structure(observer_id, 35835, corp_id, 30_000_142, "Athanor Moon Drill"),
        );
        snap.moon_extractions.insert(
            observer_id,
            MoonExtractionRecord {
                structure_id: observer_id,
                moon_id: 40_009_082,
                extraction_start_unix: now.saturating_sub(7_200),
                chunk_arrival_unix: now + 7_200,
                natural_decay_unix: now + 18_000,
                status: "Active".into(),
                auto_fracture: false,
                event_id: 1,
                moon_ores: vec![(45492, 12_000.0)],
                spawned_asteroid_ids: Vec::new(),
            },
        );
        snap.observer_mining_ledger.insert(
            observer_id,
            vec![ObserverMiningRecord {
                observer_id,
                character_id: ceo_id,
                recorded_corporation_id: corp_id,
                type_id: 45490,
                quantity: 1_500,
                timestamp_unix: now,
                last_updated_date: "2026-01-01".into(),
            }],
        );

        // Seed customs office
        snap.corp_customs_offices.insert(
            corp_id,
            vec![CustomsOfficeRecord {
                office_id: 10_040_009_082,
                system_id: 30_000_142,
                reinforce_exit_start: 18,
                reinforce_exit_end: 20,
                allow_alliance_access: true,
                allow_access_with_standings: false,
                standing_level: None,
                corporation_tax_rate: Some(0.05),
                alliance_tax_rate: Some(0.08),
                excellent_standing_tax_rate: None,
                good_standing_tax_rate: None,
                neutral_standing_tax_rate: None,
                bad_standing_tax_rate: None,
                terrible_standing_tax_rate: None,
            }],
        );
    });

    let all_scopes = vec![
        "esi-markets.read_corporation_orders.v1",
        "esi-industry.read_corporation_mining.v1",
        "esi-planets.read_customs_offices.v1",
    ];
    let tok_ceo = issue_token_with_scopes(&ctx, ceo_id, all_scopes.clone());
    let tok_no_role = issue_token_with_scopes(&ctx, no_role_id, all_scopes.clone());
    let tok_sec = issue_token_with_scopes(&ctx, sec_officer_id, all_scopes.clone());
    let tok_acct = issue_token_with_scopes(&ctx, accountant_id, all_scopes.clone());
    let tok_jr_acct = issue_token_with_scopes(&ctx, jr_accountant_id, all_scopes.clone());
    let tok_trader = issue_token_with_scopes(&ctx, trader_id, all_scopes.clone());
    let tok_station_mgr = issue_token_with_scopes(&ctx, station_mgr_id, all_scopes);

    let orders_url = format!("/v1/corporations/{corp_id}/orders/");
    let orders_hist_url = format!("/v1/corporations/{corp_id}/orders/history/");
    let extractions_url = format!("/v1/corporation/{corp_id}/mining/extractions/");
    let observers_url = format!("/v1/corporation/{corp_id}/mining/observers/");
    let observer_detail_url = format!("/v1/corporation/{corp_id}/mining/observers/{observer_id}/");
    let customs_url = format!("/v1/corporations/{corp_id}/customs_offices/");

    // Negative: zero-role & Security_Officer members receive 403 on all 6 endpoints despite valid SSO scopes
    for url in [
        &orders_url,
        &orders_hist_url,
        &extractions_url,
        &observers_url,
        &observer_detail_url,
        &customs_url,
    ] {
        assert_status(&router, url, Some(&tok_no_role), 403);
        assert_status(&router, url, Some(&tok_sec), 403);
    }

    // Orders & Order History: OpenAPI x-required-roles = ["Accountant", "Trader"] (Accountant/Trader succeed 200; Junior_Accountant/Station_Manager fail 403)
    for tok in [&tok_acct, &tok_trader] {
        let ord = get_validated_json(&router, &spec, &orders_url, Some(tok));
        assert_eq!(ord.as_array().unwrap().len(), 1);
        let hist = get_validated_json(&router, &spec, &orders_hist_url, Some(tok));
        assert_eq!(hist.as_array().unwrap().len(), 1);
    }
    for tok in [&tok_jr_acct, &tok_station_mgr] {
        assert_status(&router, &orders_url, Some(tok), 403);
        assert_status(&router, &orders_hist_url, Some(tok), 403);
    }

    // Mining Extractions: OpenAPI x-required-roles = ["Station_Manager"] (Station_Manager succeeds 200; Accountant/Trader/Junior_Accountant fail 403)
    let ext = get_validated_json(&router, &spec, &extractions_url, Some(&tok_station_mgr));
    assert_eq!(ext.as_array().unwrap().len(), 1);
    for tok in [&tok_acct, &tok_trader, &tok_jr_acct] {
        assert_status(&router, &extractions_url, Some(tok), 403);
    }

    // Mining Observers & Observer Ledger: OpenAPI x-required-roles = ["Accountant"] (Accountant succeeds 200; Station_Manager/Trader/Junior_Accountant fail 403)
    let obs = get_validated_json(&router, &spec, &observers_url, Some(&tok_acct));
    assert_eq!(obs.as_array().unwrap().len(), 1);
    let obs_det = get_validated_json(&router, &spec, &observer_detail_url, Some(&tok_acct));
    assert_eq!(obs_det.as_array().unwrap().len(), 1);
    for tok in [&tok_station_mgr, &tok_trader, &tok_jr_acct] {
        assert_status(&router, &observers_url, Some(tok), 403);
        assert_status(&router, &observer_detail_url, Some(tok), 403);
    }

    // Customs Offices: OpenAPI x-required-roles = ["Director"] (Director/CEO succeeds 200; Station_Manager/Accountant/Trader/Junior_Accountant fail 403)
    let pocos = get_validated_json(&router, &spec, &customs_url, Some(&tok_ceo));
    assert_eq!(pocos.as_array().unwrap().len(), 1);
    for tok in [&tok_station_mgr, &tok_acct, &tok_trader, &tok_jr_acct] {
        assert_status(&router, &customs_url, Some(tok), 403);
    }
}
