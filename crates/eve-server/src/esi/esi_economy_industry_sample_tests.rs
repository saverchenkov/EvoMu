//! Comprehensive unit and mutation-killing tests for ESI economy, industry, contracts, mining, and sample_routes.

use super::full_endpoint_catalog_tests::seed_full_esi_context;
use super::sample_routes::{format_iso8601_utc, paginate_slice};
use super::{EsiHttpRequest, EsiRouter};
use eve_db::{
    IncursionRecord, IndustryJobRecord, InventoryFlag, InventoryItemRecord, IskCents,
    KillmailAttackerDetail, KillmailDetailRecord, LoyaltyStoreOfferItemRecord,
    LoyaltyStoreOfferRecord, MarketOrderMeta, MarketOrderRecord, MiningLedgerRecord,
    MoonExtractionRecord, ObserverMiningRecord,
};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;

fn call_json(
    router: &EsiRouter,
    method: &str,
    path: &str,
    token: &str,
    body: Option<Value>,
) -> (u16, Value) {
    let mut req = EsiHttpRequest::new(method, path).with_bearer_token(token);
    if let Some(b) = body {
        req = req
            .with_header("Content-Type", "application/json")
            .with_body(serde_json::to_vec(&b).unwrap());
    }
    let resp = router.handle_request(&req);
    let val = if resp.body.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&resp.body).unwrap_or(Value::Null)
    };
    (resp.status_code, val)
}

#[test]
fn sample_routes_helpers_iso8601_pagination_dogma_killmails_wars_route_and_ui() {
    assert_eq!(format_iso8601_utc(0), "1970-01-01T00:00:00Z");
    assert_eq!(format_iso8601_utc(1_709_210_096), "2024-02-29T12:34:56Z");

    let items = vec![10, 20, 30, 40, 50];
    assert_eq!(paginate_slice(&items, 0, 2).unwrap_err().status_code, 400);
    assert_eq!(paginate_slice(&items, 4, 2).unwrap_err().status_code, 404);
    let (p1, total) = paginate_slice(&items, 1, 2).unwrap();
    assert_eq!((p1, total), (vec![10, 20], 3));
    let (p3, _) = paginate_slice(&items, 3, 2).unwrap();
    assert_eq!(p3, vec![50]);
    let empty: Vec<i32> = Vec::new();
    let (pe, te) = paginate_slice(&empty, 1, 10).unwrap();
    assert!(pe.is_empty() && te == 1);

    let (ctx, token, char_id, corp_id, _planet_id) = seed_full_esi_context();
    let router = EsiRouter::new(Arc::clone(&ctx));
    let now = ctx.now_unix();
    let abyssal_item_id = 1_090_000_001_i64;

    ctx.db.with_mut_snapshot(|snap| {
        snap.incursions.insert(
            20_000_020,
            IncursionRecord {
                constellation_id: 20_000_020,
                staging_solar_system_id: 30_000_142,
                infested_solar_systems: vec![30_000_142, 30_000_144],
                faction_id: 500_019,
                has_boss: true,
                influence: 0.75,
                state: "mobilizing".into(),
                incursion_type: "Incursion".into(),
            },
        );
        snap.items.insert(
            abyssal_item_id,
            InventoryItemRecord {
                item_id: abyssal_item_id,
                type_id: 47_740,
                owner_id: char_id,
                location_id: 60_003_760,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                dynamic_attributes: HashMap::from([(54, 15_000.0), (6, 45.0)]),
                source_mutaplasmid_id: Some(47_745),
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
        snap.killmail_details.insert(
            55_001,
            KillmailDetailRecord {
                kill_id: 55_001,
                killmail_time_unix: now.saturating_sub(100),
                damage_taken: 1_250,
                victim_position: Some([100.0, 200.0, 300.0]),
                attackers: vec![KillmailAttackerDetail {
                    character_id: char_id,
                    corporation_id: corp_id,
                    alliance_id: Some(99_000_001),
                    ship_type_id: 587,
                    weapon_type_id: 2897,
                    damage_done: 1_250,
                    final_blow: true,
                    security_status: 2.5,
                }],
            },
        );
    });

    // Status and Meta compatibility-dates
    let (_, st_norm) = call_json(&router, "GET", "/latest/status", &token, None);
    assert_eq!(st_norm["vip"], json!(false));
    let (_, compat) = call_json(&router, "GET", "/latest/meta/compatibility-dates", &token, None);
    assert!(!compat["compatibility_dates"].as_array().unwrap().is_empty());

    // Incursions & Insurance
    let (_, inc) = call_json(&router, "GET", "/latest/incursions", &token, None);
    assert_eq!(inc[0]["constellation_id"], json!(20_000_020));
    let (_, ins) = call_json(&router, "GET", "/latest/insurance/prices", &token, None);
    assert!(!ins.as_array().unwrap().is_empty());

    // Dogma attributes, effects, and dynamic items (200 vs 404)
    assert_eq!(
        call_json(&router, "GET", "/latest/dogma/attributes/999999", &token, None).0,
        404
    );
    assert_eq!(
        call_json(&router, "GET", "/latest/dogma/effects/999999", &token, None).0,
        404
    );
    assert_eq!(
        call_json(
            &router,
            "GET",
            &format!("/latest/dogma/dynamic/items/587/{abyssal_item_id}"),
            &token,
            None,
        )
        .0,
        404
    );
    let (st_dyn, dyn_val) = call_json(
        &router,
        "GET",
        &format!("/latest/dogma/dynamic/items/47740/{abyssal_item_id}"),
        &token,
        None,
    );
    assert_eq!(st_dyn, 200);
    assert_eq!(dyn_val["mutator_type_id"], json!(47_745));

    // Killmails (404, 422 wrong hash, 200 valid hash)
    let (_, recent_kms) = call_json(
        &router,
        "GET",
        &format!("/latest/characters/{char_id}/killmails/recent"),
        &token,
        None,
    );
    let valid_hash = recent_kms[0]["killmail_hash"].as_str().unwrap().to_string();
    assert_eq!(
        call_json(&router, "GET", "/latest/killmails/999999/deadbeef", &token, None).0,
        404
    );
    assert_eq!(
        call_json(&router, "GET", "/latest/killmails/55001/wronghash", &token, None).0,
        422
    );
    let (st_km, km_val) = call_json(
        &router,
        "GET",
        &format!("/latest/killmails/55001/{valid_hash}"),
        &token,
        None,
    );
    assert_eq!(st_km, 200);
    assert_eq!(km_val["victim"]["damage_taken"], json!(1_250));

    // Route planner flags, avoid, connections, and error branches
    assert_eq!(
        call_json(&router, "GET", "/latest/route/99999999/30000142", &token, None).0,
        404
    );
    assert_eq!(
        call_json(
            &router,
            "GET",
            "/latest/route/30000142/30000144?flag=invalid",
            &token,
            None,
        )
        .0,
        400
    );
    for flag in ["shortest", "secure", "insecure"] {
        let (st_r, r_val) = call_json(
            &router,
            "GET",
            &format!("/latest/route/30000142/30000144?flag={flag}&connections=30000142|30000144"),
            &token,
            None,
        );
        assert_eq!(st_r, 200);
        assert_eq!(r_val, json!([30_000_142, 30_000_144]));
    }

    // UI openwindow endpoints: offline -> 400, online missing params -> 400, online valid -> 204
    assert_eq!(
        call_json(
            &router,
            "POST",
            "/latest/ui/openwindow/contract?contract_id=66001",
            &token,
            None,
        )
        .0,
        400
    );
    let uid = ctx.db.with_snapshot(|s| s.characters[&char_id].user_id);
    let _ = ctx
        .db
        .acquire_online_lock(uid, 999, Some(char_id), false, ctx.now_unix());
    assert_eq!(
        call_json(&router, "POST", "/latest/ui/openwindow/contract", &token, None).0,
        400
    );
    assert_eq!(
        call_json(
            &router,
            "POST",
            "/latest/ui/openwindow/contract?contract_id=66001",
            &token,
            None,
        )
        .0,
        204
    );
    assert_eq!(
        call_json(
            &router,
            "POST",
            &format!("/latest/ui/openwindow/information?target_id={char_id}"),
            &token,
            None,
        )
        .0,
        204
    );
    assert_eq!(
        call_json(
            &router,
            "POST",
            "/latest/ui/openwindow/information?target_id=30000142",
            &token,
            None,
        )
        .0,
        501
    );
    assert_eq!(
        call_json(
            &router,
            "POST",
            "/latest/ui/openwindow/information?target_id=999999999",
            &token,
            None,
        )
        .0,
        404
    );
    assert_eq!(
        call_json(
            &router,
            "POST",
            "/latest/ui/openwindow/marketdetails?type_id=34",
            &token,
            None,
        )
        .0,
        204
    );
    assert_eq!(
        call_json(
            &router,
            "POST",
            "/latest/ui/openwindow/newmail",
            &token,
            Some(json!({
                "subject": "Hi",
                "body": "Hello",
                "recipients": [char_id]
            })),
        )
        .0,
        204
    );
}

#[test]
fn economy_and_industry_routes_contracts_jobs_mining_markets_and_loyalty() {
    let (ctx, token, char_id, corp_id, _planet_id) = seed_full_esi_context();
    let router = EsiRouter::new(Arc::clone(&ctx));
    let now = ctx.now_unix();
    let struct_id = 1_055_000_001_i64;

    ctx.db.with_mut_snapshot(|snap| {
        // Completed industry job to test include_completed=true vs false
        snap.industry_jobs.insert(
            77_002,
            IndustryJobRecord {
                job_id: 77_002,
                installer_id: char_id,
                facility_id: struct_id,
                blueprint_item_id: 100_000_051,
                blueprint_type_id: 681,
                activity_id: 8,
                runs: 1,
                product_type_id: 587,
                status: "Delivered".into(),
                duration_seconds: 1_800,
                start_unix: now.saturating_sub(3600),
                blueprint_location_id: struct_id,
                output_location_id: struct_id,
                cost: Some(500.0),
                licensed_runs: Some(5),
                probability: Some(0.45),
                completed_unix: Some(now.saturating_sub(1800)),
                completed_character_id: Some(char_id),
            },
        );
        // Mining ledger, moon extraction, observer mining
        snap.mining_ledger.push(MiningLedgerRecord {
            character_id: char_id,
            solar_system_id: 30_000_142,
            type_id: 1228,
            quantity: 2_500,
            date: "2026-03-20".into(),
            timestamp_unix: now,
        });
        snap.moon_extractions.insert(
            struct_id,
            MoonExtractionRecord {
                structure_id: struct_id,
                moon_id: 40_009_049,
                extraction_start_unix: now.saturating_sub(3600),
                chunk_arrival_unix: now + 3600,
                natural_decay_unix: now + 10_800,
                status: "Started".into(),
                auto_fracture: true,
                event_id: 10,
                moon_ores: vec![(45_490, 1.0)],
                spawned_asteroid_ids: vec![],
            },
        );
        snap.observer_mining_ledger.insert(
            struct_id,
            vec![ObserverMiningRecord {
                observer_id: struct_id,
                character_id: char_id,
                recorded_corporation_id: corp_id,
                type_id: 45_490,
                quantity: 1_200,
                last_updated_date: "2026-03-20".into(),
                timestamp_unix: now,
            }],
        );
        // Cancelled/expired market orders for char and corp orders/history (vol_remaining == 0)
        snap.market_orders.insert(
            88_002,
            MarketOrderRecord {
                order_id: 88_002,
                owner_id: char_id,
                station_id: struct_id,
                region_id: 10_000_002,
                type_id: 34,
                is_bid: true,
                price: IskCents::from_isk(4),
                vol_entered: 500,
                vol_remaining: 0,
                min_volume: 1,
                is_npc: false,
                escrow: IskCents::from_isk(0),
            },
        );
        snap.market_order_meta.insert(
            88_002,
            MarketOrderMeta {
                issued_unix: now.saturating_sub(600),
                duration_days: 30,
                range: "region".into(),
                issued_by: Some(char_id),
                wallet_division: Some(1),
                is_corporation: false,
                state: Some("cancelled".into()),
            },
        );
        snap.market_orders.insert(
            88_003,
            MarketOrderRecord {
                order_id: 88_003,
                owner_id: corp_id,
                station_id: struct_id,
                region_id: 10_000_002,
                type_id: 34,
                is_bid: true,
                price: IskCents::from_isk(4),
                vol_entered: 500,
                vol_remaining: 0,
                min_volume: 1,
                is_npc: false,
                escrow: IskCents::from_isk(0),
            },
        );
        snap.market_order_meta.insert(
            88_003,
            MarketOrderMeta {
                issued_unix: now.saturating_sub(600),
                duration_days: 30,
                range: "region".into(),
                issued_by: Some(char_id),
                wallet_division: Some(1),
                is_corporation: true,
                state: Some("cancelled".into()),
            },
        );
        // Loyalty store offer for NPC corp 1000125
        snap.loyalty_store_offers.insert(
            1_000_125,
            vec![LoyaltyStoreOfferRecord {
                offer_id: 901,
                type_id: 587,
                quantity: 1,
                lp_cost: 5_000,
                isk_cost: 1_000_000,
                ak_cost: Some(10),
                required_items: vec![LoyaltyStoreOfferItemRecord {
                    type_id: 34,
                    quantity: 100,
                }],
            }],
        );
    });

    // Industry jobs include_completed filter
    let (_, jobs_active) = call_json(
        &router,
        "GET",
        &format!("/latest/characters/{char_id}/industry/jobs?include_completed=false"),
        &token,
        None,
    );
    assert_eq!(jobs_active.as_array().unwrap().len(), 1);
    let (_, jobs_all) = call_json(
        &router,
        "GET",
        &format!("/latest/characters/{char_id}/industry/jobs?include_completed=true"),
        &token,
        None,
    );
    assert_eq!(jobs_all.as_array().unwrap().len(), 2);

    // Mining ledger, moon extractions, observers
    let (_, m_led) = call_json(
        &router,
        "GET",
        &format!("/latest/characters/{char_id}/mining"),
        &token,
        None,
    );
    assert_eq!(m_led[0]["quantity"], json!(2_500));

    let (_, m_ext) = call_json(
        &router,
        "GET",
        &format!("/latest/corporation/{corp_id}/mining/extractions"),
        &token,
        None,
    );
    assert_eq!(m_ext[0]["structure_id"], json!(struct_id));

    let (_, m_obs) = call_json(
        &router,
        "GET",
        &format!("/latest/corporation/{corp_id}/mining/observers"),
        &token,
        None,
    );
    assert_eq!(m_obs[0]["observer_id"], json!(struct_id));

    let (_, m_obs_det) = call_json(
        &router,
        "GET",
        &format!("/latest/corporation/{corp_id}/mining/observers/{struct_id}"),
        &token,
        None,
    );
    assert_eq!(m_obs_det[0]["quantity"], json!(1_200));

    // Market orders, history, structure orders, and loyalty store offers
    assert_eq!(
        call_json(
            &router,
            "GET",
            "/latest/markets/10000002/orders?order_type=bad",
            &token,
            None,
        )
        .0,
        400
    );
    assert_eq!(
        call_json(&router, "GET", "/latest/markets/10000002/history", &token, None).0,
        400
    );
    let (_, ord_hist) = call_json(
        &router,
        "GET",
        &format!("/latest/characters/{char_id}/orders/history"),
        &token,
        None,
    );
    assert_eq!(ord_hist[0]["order_id"], json!(88_002));

    let (_, corp_ord_hist) = call_json(
        &router,
        "GET",
        &format!("/latest/corporations/{corp_id}/orders/history"),
        &token,
        None,
    );
    assert_eq!(corp_ord_hist[0]["order_id"], json!(88_003));

    let (st_lp, lp_offers) = call_json(
        &router,
        "GET",
        "/latest/loyalty/stores/1000125/offers",
        &token,
        None,
    );
    assert_eq!(st_lp, 200);
    assert_eq!(lp_offers[0]["offer_id"], json!(901));
    assert_eq!(lp_offers[0]["ak_cost"], json!(10));
}
