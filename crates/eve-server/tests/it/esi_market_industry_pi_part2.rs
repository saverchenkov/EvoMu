//! Remediation integration tests for:
//! - ESI Market Groups localized SDE descriptions (`esi:GET /markets/groups/{market_group_id}`, `feat:esi.market.groups`)
//! - ESI Structure Market Orders Hub & ACL enforcement (`esi:GET /markets/structures/{structure_id}`, `feat:esi.market.structure-orders`)
//! - ESI Corporation endpoint-specific OpenAPI role gates & route registration across all 6 corp endpoints

#![forbid(unsafe_code)]

use crate::esi_market_industry_pi::{
    assert_status, get_json, issue_token_with_scopes, make_item, make_structure, rpc_call,
};
use eve_db::{
    corp_roles, CorporationRecord, InventoryFlag, IskCents, UpwellStructureRecord,
};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::esi::{EsiRouter, OpenApiSpec};
use eve_server::router::ServerContext;
use eve_server::services::station_structure::structure_upwell_ops::{
    load_ext_state, save_ext_state, StructureAccessProfile,
};
use eve_destiny::{Ball, MovementMode};
use glam::DVec3;
use serde_json::json;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

#[tokio::test]
async fn test_esi_market_groups_sde_descriptions_and_structure_orders_acl_hub() {
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

    // 1. Verify market group descriptions come from V24 marketgroups.fsdbinary + localization pickle (not name)
    for mg_id in [4u32, 9u32] {
        let expected_desc = ctx
            .sde
            .market_group_description(mg_id)
            .expect("SDE localized market group description");
        assert!(!expected_desc.is_empty());
        let detail = get_json(&router, &spec, format!("/v1/markets/groups/{mg_id}/"), None);
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
            ch.station_id = None;
            ch.structure_id = Some(fortizar_hub_id);
            ch.solar_system_id = 30_000_142;
            ch.balance = IskCents::from_isk(100_000_000);
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
            make_item(950_001, 35_892, corp_id, fortizar_hub_id, InventoryFlag::ServiceSlot0, 1, true),
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
            make_item(
                950_002,
                35_892,
                corp_id,
                offline_hub_fortizar_id,
                InventoryFlag::ServiceSlot0,
                1,
                true,
            ),
        );
        snap.module_online_states.insert(950_002, false);

        // (d) Fortizar with online Market Hub but docking ACL denied for caller
        snap.structures.insert(
            acl_blocked_fortizar_id,
            make_structure(acl_blocked_fortizar_id, 35833, corp_id, 30_000_142, "ACL Blocked Fortizar"),
        );
        snap.items.insert(
            950_003,
            make_item(
                950_003,
                35_892,
                corp_id,
                acl_blocked_fortizar_id,
                InventoryFlag::ServiceSlot0,
                1,
                true,
            ),
        );
        let mut ext = load_ext_state(snap);
        ext.access_profiles.insert(
            acl_blocked_fortizar_id,
            StructureAccessProfile {
                profile_id: 1,
                owner_corp_id: corp_id,
                blocked_characters: HashSet::from([char_id]),
                ..Default::default()
            },
        );
        save_ext_state(snap, &ext);
    });

    // Place a real buy order at fortizar_hub_id via marketProxy.PlaceBuyOrder
    let mut sess = SessionState::new(9851);
    sess.user_id = Some(1);
    sess.character_id = Some(char_id);
    sess.corporation_id = Some(corp_id);
    sess.structure_id = Some(fortizar_hub_id);
    sess.solar_system_id = Some(30_000_142);
    sess.region_id = Some(10_000_002);
    let buy_args = [
        PyRep::Integer(fortizar_hub_id),
        PyRep::Integer(34),
        PyRep::Real(8.0),
        PyRep::Integer(1_000),
        PyRep::Integer(-1),
        PyRep::Integer(1),
        PyRep::Integer(30),
        PyRep::Bool(false),
    ];
    let buy_rep = rpc_call(&ctx, &mut sess, "marketProxy", "PlaceBuyOrder", &buy_args).await;
    let placed_oid = buy_rep.dict_get("orderID").and_then(|v| v.as_i64()).expect("orderID");

    let token = issue_token_with_scopes(&ctx, char_id, vec!["esi-markets.structure_markets.v1"]);
    let ok_orders = get_json(
        &router,
        &spec,
        format!("/v1/markets/structures/{fortizar_hub_id}/"),
        Some(&token),
    );
    assert_eq!(ok_orders.as_array().unwrap().len(), 1);
    assert_eq!(ok_orders[0]["order_id"], json!(placed_oid));

    assert_status(&router, format!("/v1/markets/structures/{bare_astrahus_id}/"), Some(&token), 403);
    assert_status(
        &router,
        format!("/v1/markets/structures/{offline_hub_fortizar_id}/"),
        Some(&token),
        403,
    );
    assert_status(
        &router,
        format!("/v1/markets/structures/{acl_blocked_fortizar_id}/"),
        Some(&token),
        403,
    );
}

#[tokio::test]
async fn test_esi_corp_endpoint_specific_role_gates_and_routes() {
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

    let corp_id = 98_000_722_i64;
    let ceo_id = 90_000_001_i64;
    let no_role_id = 90_000_721_i64;
    let sec_officer_id = 90_000_722_i64;
    let accountant_id = 90_000_723_i64;
    let jr_accountant_id = 90_000_724_i64;
    let trader_id = 90_000_725_i64;
    let station_mgr_id = 90_000_726_i64;
    let observer_id = 1_000_000_722_001_i64;
    let ship_id = 100_000_001_i64;
    let jita_station = 60_003_760_i64;
    let lowsec_sys = 30_000_192_u32;
    let lowsec_planet_id = ctx.sde.solar_systems[&lowsec_sys].planet_ids[0];
    let perim_moon = ctx
        .sde
        .celestials_in_system(30_000_144)
        .into_iter()
        .find(|c| c.group_id == 8)
        .expect("SDE moon in Perimeter");
    let perim_pos = [
        perim_moon.position.x + 2_000.0,
        perim_moon.position.y,
        perim_moon.position.z,
    ];

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
            ch.station_id = None;
            ch.structure_id = None;
            ch.solar_system_id = 30_000_144;
            ch.active_ship_id = ship_id;
            ch.balance = IskCents::from_isk(250_000_000);
            snap.characters.insert(cid, ch);
        }
        let mut corp = CorporationRecord {
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
                (
                    ceo_id,
                    corp_roles::ROLE_DIRECTOR
                        | corp_roles::ROLE_STATION_MANAGER
                        | corp_roles::ROLE_ACCOUNTANT
                        | corp_roles::ROLE_TRADER,
                ),
                (no_role_id, 0),
                (sec_officer_id, corp_roles::ROLE_SECURITY_OFFICER),
                (accountant_id, corp_roles::ROLE_ACCOUNTANT),
                (jr_accountant_id, corp_roles::ROLE_JUNIOR_ACCOUNTANT),
                (trader_id, corp_roles::ROLE_TRADER),
                (station_mgr_id, corp_roles::ROLE_STATION_MANAGER),
            ]),
            master_wallet: IskCents::from_isk(250_000_000),
            ..Default::default()
        };
        corp.ensure_wallet_divisions();
        for d in &mut corp.wallet_divisions {
            d.balance = IskCents::from_isk(250_000_000);
        }
        snap.corporations.insert(corp_id, corp);

        // Refinery with online Standup Moon Drill I (35894) + Gantry/POCO items in cargo
        snap.structures.insert(
            observer_id,
            UpwellStructureRecord {
                structure_id: observer_id,
                type_id: 35835,
                owner_corp_id: corp_id,
                solar_system_id: 30_000_144,
                name: "Athanor Moon Drill".into(),
                position: perim_pos,
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 1_000,
                shield_hp: 10000.0,
                armor_hp: 10000.0,
                hull_hp: 10000.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: None,
            },
        );
        snap.items.insert(
            951_001,
            make_item(951_001, 35_894, corp_id, observer_id, InventoryFlag::ServiceSlot0, 1, true),
        );
    });

    // 1. Populate moon extraction & observer mining ledger via live RPCs
    let mut ceo_sess = SessionState::new(9852);
    ceo_sess.user_id = Some(1);
    ceo_sess.character_id = Some(ceo_id);
    ceo_sess.corporation_id = Some(corp_id);
    ceo_sess.corp_role = corp_roles::ROLE_DIRECTOR
        | corp_roles::ROLE_STATION_MANAGER
        | corp_roles::ROLE_ACCOUNTANT
        | corp_roles::ROLE_TRADER;
    ceo_sess.solar_system_id = Some(30_000_144);
    ceo_sess.ship_id = Some(ship_id);

    let ext_args = [PyRep::Integer(observer_id), PyRep::Integer(518_400)];
    rpc_call(&ctx, &mut ceo_sess, "moonExtractions", "StartNewExtraction", &ext_args).await;
    ctx.db.clock().advance_secs(520_000);
    let frac = rpc_call(
        &ctx,
        &mut ceo_sess,
        "moonExtractions",
        "FractureChunkWithStructure",
        &[PyRep::Integer(observer_id)],
    )
    .await;
    let ast_id = match frac.dict_get("asteroidIDs") {
        Some(PyRep::List(v)) => v[0].as_i64().expect("fractured asteroid ID"),
        other => panic!("unexpected asteroidIDs: {other:?}"),
    };
    let _ = ctx
        .systems
        .get_or_spawn(30_000_144)
        .spawn_ball(Ball {
            id: ship_id,
            type_id: 606,
            position: DVec3::from_array(perim_pos),
            velocity: DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 1.0,
            mass: 1_000_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;
    rpc_call(&ctx, &mut ceo_sess, "dogmaIM", "Mine", &[PyRep::Integer(ast_id)]).await;
    rpc_call(&ctx, &mut ceo_sess, "moonExtractions", "StartNewExtraction", &ext_args).await;

    // 2. Populate active & historical corporate orders via marketProxy.PlaceBuyOrder + CancelCharOrder
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&ceo_id) {
            ch.station_id = Some(jita_station);
            ch.solar_system_id = 30_000_142;
        }
    });
    ceo_sess.station_id = Some(jita_station);
    ceo_sess.solar_system_id = Some(30_000_142);
    ceo_sess.region_id = Some(10_000_002);
    let corp_buy_args = [
        PyRep::Integer(jita_station),
        PyRep::Integer(36),
        PyRep::Real(25.0),
        PyRep::Integer(500),
        PyRep::Integer(-1),
        PyRep::Integer(1),
        PyRep::Integer(30),
        PyRep::Bool(true),
        PyRep::Integer(1000),
    ];
    rpc_call(&ctx, &mut ceo_sess, "marketProxy", "PlaceBuyOrder", &corp_buy_args).await;
    let hist_rep = rpc_call(&ctx, &mut ceo_sess, "marketProxy", "PlaceBuyOrder", &corp_buy_args).await;
    let hist_oid = hist_rep.dict_get("orderID").and_then(|v| v.as_i64()).unwrap();
    rpc_call(&ctx, &mut ceo_sess, "marketProxy", "CancelCharOrder", &[PyRep::Integer(hist_oid)])
        .await;

    // 3. Populate customs office via posMgr.AnchorOrbital + OnlineOrbital + planetOrbitalRegistryBroker.UpdateSettings
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&ceo_id) {
            ch.station_id = None;
            ch.solar_system_id = lowsec_sys;
            ch.standings.insert(500_026, 2.0);
        }
        snap.items
            .insert(951_002, make_item(951_002, 3962, corp_id, ship_id, InventoryFlag::Cargo, 1, false));
        snap.items
            .insert(951_003, make_item(951_003, 2233, corp_id, ship_id, InventoryFlag::Cargo, 1, false));
    });
    ceo_sess.station_id = None;
    ceo_sess.solar_system_id = Some(lowsec_sys);
    rpc_call(
        &ctx,
        &mut ceo_sess,
        "posMgr",
        "AnchorOrbital",
        &[PyRep::Integer(951_002), PyRep::Integer(lowsec_planet_id)],
    )
    .await;
    rpc_call(
        &ctx,
        &mut ceo_sess,
        "posMgr",
        "OnlineOrbital",
        &[PyRep::Integer(951_002), PyRep::Integer(951_003)],
    )
    .await;
    let poco_args = [
        PyRep::Integer(lowsec_planet_id),
        PyRep::Integer(18),
        PyRep::keyval([
            ("corporation", PyRep::Real(0.05)),
            ("alliance", PyRep::Real(0.08)),
            ("standingHigh", PyRep::Real(0.05)),
            ("standingGood", PyRep::Real(0.08)),
            ("standingNeutral", PyRep::Real(0.10)),
            ("standingBad", PyRep::Real(0.15)),
            ("standingHorrible", PyRep::Real(0.20)),
        ]),
        PyRep::Real(0.0),
        PyRep::Bool(true),
        PyRep::Bool(false),
    ];
    rpc_call(&ctx, &mut ceo_sess, "planetOrbitalRegistryBroker", "UpdateSettings", &poco_args)
        .await;

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

    // Orders & Order History: OpenAPI x-required-roles = ["Accountant", "Trader"]
    for tok in [&tok_acct, &tok_trader] {
        let ord = get_json(&router, &spec, &orders_url, Some(tok));
        assert_eq!(ord.as_array().unwrap().len(), 1);
        let hist = get_json(&router, &spec, &orders_hist_url, Some(tok));
        assert_eq!(hist.as_array().unwrap().len(), 1);
    }
    for tok in [&tok_jr_acct, &tok_station_mgr] {
        assert_status(&router, &orders_url, Some(tok), 403);
        assert_status(&router, &orders_hist_url, Some(tok), 403);
    }

    // Mining Extractions: OpenAPI x-required-roles = ["Station_Manager"]
    let ext = get_json(&router, &spec, &extractions_url, Some(&tok_station_mgr));
    assert_eq!(ext.as_array().unwrap().len(), 1);
    for tok in [&tok_acct, &tok_trader, &tok_jr_acct] {
        assert_status(&router, &extractions_url, Some(tok), 403);
    }

    // Mining Observers & Observer Ledger: OpenAPI x-required-roles = ["Accountant"]
    let obs = get_json(&router, &spec, &observers_url, Some(&tok_acct));
    assert_eq!(obs.as_array().unwrap().len(), 1);
    let obs_det = get_json(&router, &spec, &observer_detail_url, Some(&tok_acct));
    assert_eq!(obs_det.as_array().unwrap().len(), 1);
    for tok in [&tok_station_mgr, &tok_trader, &tok_jr_acct] {
        assert_status(&router, &observers_url, Some(tok), 403);
        assert_status(&router, &observer_detail_url, Some(tok), 403);
    }

    // Customs Offices: OpenAPI x-required-roles = ["Director"]
    let pocos = get_json(&router, &spec, &customs_url, Some(&tok_ceo));
    assert_eq!(pocos.as_array().unwrap().len(), 1);
    for tok in [&tok_station_mgr, &tok_acct, &tok_trader, &tok_jr_acct] {
        assert_status(&router, &customs_url, Some(tok), 403);
    }
}
