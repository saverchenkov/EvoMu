//! Full catalog and HTTP protocol coverage tests for ESI (`esi::router`, `esi::openapi`, `esi::sample_routes`, `esi::universe_routes`).

use super::{EsiHttpRequest, EsiRouter, OpenApiSpec};
use crate::router::ServerContext;
use eve_db::{
    AllianceRecord, ContractRecord, CorpWalletJournalRecord, FwCharacterStatsRecord,
    FwWindowedStat, IndustryJobRecord, InventoryFlag, InventoryItemRecord, IskCents,
    KillmailRecord, MarketOrderRecord, PlanetaryColonyRecord, SovCampaignScoreRecord,
    UpwellStructureRecord, WalletJournalEntry, WalletTransactionRecord, WarAllyRecord,
    WarLifecycleMeta, WarRecord,
};
use eve_sde::SdeRegistry;
use serde_json::json;
use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, OnceLock};

static SHARED_SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();

pub(crate) fn seed_full_esi_context() -> (Arc<ServerContext>, String, i64, i64, u32) {
    let ctx = if let Some(sde) = SHARED_SDE.get() {
        ServerContext::with_sde_and_repository(
            Arc::clone(sde),
            Arc::new(eve_db::WorldRepository::with_demo_seed()),
        )
        .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED)
    } else {
        let c = ServerContext::new();
        let _ = SHARED_SDE.set(Arc::clone(&c.sde));
        c
    };

    let char_id = 90_000_001_i64;
    let char2_id = 90_000_002_i64;
    let corp_id = 98_000_001_i64;
    let alliance_id = 99_000_001_i64;
    let structure_id = 1_055_000_001_i64;
    let planet_id = ctx
        .sde
        .celestials_in_system(30_000_142)
        .iter()
        .find(|c| c.group_id == 7)
        .map(|c| c.celestial_id)
        .unwrap_or(40_009_048);
    let now = ctx.now_unix();

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.corporation_id = corp_id;
            ch.alliance_id = Some(alliance_id);
        }
        if let Some(ch2) = snap.characters.get_mut(&char2_id) {
            ch2.corporation_id = corp_id;
            ch2.alliance_id = Some(alliance_id);
        }
        if !snap.corporations.contains_key(&corp_id) {
            if let Some(mut corp_tpl) = snap.corporations.values().next().cloned() {
                corp_tpl.corporation_id = corp_id;
                corp_tpl.name = "Test Corporation".into();
                corp_tpl.ticker = "TSTC".into();
                corp_tpl.ceo_id = char_id;
                corp_tpl.alliance_id = Some(alliance_id);
                corp_tpl.members.insert(char_id);
                corp_tpl.members.insert(char2_id);
                corp_tpl.member_roles.insert(char_id, !0_u64);
                snap.corporations.insert(corp_id, corp_tpl);
            }
        }
        if let Some(corp) = snap.corporations.get_mut(&corp_id) {
            corp.ceo_id = char_id;
            corp.alliance_id = Some(alliance_id);
            corp.members.insert(char_id);
            corp.members.insert(char2_id);
            corp.member_roles.insert(char_id, !0_u64);
        }
        snap.alliances.insert(
            alliance_id,
            AllianceRecord {
                alliance_id,
                name: "Test Alliance".into(),
                short_name: "TSTA".into(),
                executor_corp_id: corp_id,
                member_corp_ids: [corp_id].into_iter().collect(),
            },
        );
        snap.structures.insert(
            structure_id,
            UpwellStructureRecord {
                structure_id,
                type_id: 35_832,
                owner_corp_id: corp_id,
                solar_system_id: 30_000_142,
                name: "Perimeter Keepstar".into(),
                position: [10_000.0, 0.0, 0.0],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 500,
                shield_hp: 100_000.0,
                armor_hp: 100_000.0,
                hull_hp: 100_000.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: None,
            },
        );
        snap.wars.insert(
            44_001,
            WarRecord {
                war_id: 44_001,
                aggressor_corp_id: corp_id,
                defender_corp_id: 98_000_002,
                mutual: false,
                started_unix: now.saturating_sub(500),
                active: true,
            },
        );
        snap.war_lifecycle_meta.insert(
            44_001,
            WarLifecycleMeta {
                declared_unix: Some(now.saturating_sub(600)),
                started_unix: Some(now.saturating_sub(500)),
                retracted_unix: None,
                finished_unix: None,
                open_for_allies: Some(true),
                allies: vec![WarAllyRecord {
                    corporation_id: Some(1_000_125),
                    alliance_id: None,
                }],
            },
        );
        snap.killmails.insert(
            55_001,
            KillmailRecord {
                kill_id: 55_001,
                victim_character_id: char2_id,
                attacker_character_id: char_id,
                solar_system_id: 30_000_142,
                ship_type_id: 587,
                total_value_isk: 250_000.0,
                killed_unix: now.saturating_sub(100),
                victim_corp_id: corp_id,
                victim_alliance_id: Some(alliance_id),
                attacker_corp_id: corp_id,
                attacker_alliance_id: Some(alliance_id),
            },
        );
        snap.market_orders.insert(
            88_001,
            MarketOrderRecord {
                order_id: 88_001,
                owner_id: char_id,
                station_id: 60_003_760,
                region_id: 10_000_002,
                type_id: 34,
                is_bid: false,
                price: IskCents::from_isk(5),
                vol_entered: 1_000,
                vol_remaining: 500,
                min_volume: 1,
                is_npc: false,
                escrow: IskCents(0),
            },
        );
        snap.industry_jobs.insert(
            77_001,
            IndustryJobRecord {
                job_id: 77_001,
                installer_id: char_id,
                facility_id: 60_003_760,
                blueprint_item_id: 100_000_050,
                blueprint_type_id: 681,
                activity_id: 1,
                runs: 2,
                product_type_id: 587,
                status: "Active".into(),
                duration_seconds: 3_600,
                start_unix: now.saturating_sub(100),
                blueprint_location_id: 60_003_760,
                output_location_id: 60_003_760,
                cost: Some(1_000.0),
                licensed_runs: Some(10),
                probability: Some(1.0),
                completed_unix: None,
                completed_character_id: None,
            },
        );
        snap.contracts.insert(
            66_001,
            ContractRecord {
                contract_id: 66_001,
                issuer_id: char_id,
                acceptor_id: None,
                contract_type: "Auction".into(),
                status: "Outstanding".into(),
                start_station_id: 60_003_760,
                end_station_id: 60_003_760,
                price: IskCents::from_isk(100_000),
                reward: IskCents(0),
                collateral: IskCents(0),
                item_ids: vec![100_000_001],
                expiration_unix: now + 86_400,
                issuer_corp_id: corp_id,
                assignee_id: 0,
                availability: "Public".into(),
                buyout: IskCents::from_isk(500_000),
                current_bid: IskCents::from_isk(120_000),
                current_bidder_id: Some(char2_id),
                requested_items: vec![(34, 100)],
                bid_history: vec![(char2_id, IskCents::from_isk(120_000), now)],
                date_issued_unix: now,
                title: "Public Rifter Auction".into(),
                for_corp: true,
                num_days: 7,
            },
        );
        snap.planetary_colonies.insert(
            (char_id, planet_id),
            PlanetaryColonyRecord {
                planet_id,
                owner_id: char_id,
                solar_system_id: 30_000_142,
                command_center_level: 3,
                pins_count: 4,
                extracted_commodities: HashMap::from([(2073, 500)]),
                last_extracted_unix: now,
            },
        );
        snap.cluster_and_runtime
            .portraits
            .insert(format!("{char_id}:64"), "ffd8ffe0".into());
        snap.wallet_journal.push(WalletJournalEntry {
            entry_id: 99_001,
            character_id: char_id,
            entry_type: "bounty_prizes".into(),
            amount: IskCents::from_isk(50_000),
            balance_after: IskCents::from_isk(1_050_000),
            reason: "Rat bounty".into(),
        });
        snap.char_wallet_transactions
            .entry(char_id)
            .or_default()
            .push(WalletTransactionRecord {
                transaction_id: 99_101,
                date_unix: now,
                location_id: 60_003_760,
                type_id: 34,
                unit_price: 5.0,
                quantity: 100,
                client_id: char2_id,
                is_buy: true,
                is_personal: true,
                journal_ref_id: 99_001,
            });
        snap.corp_wallet_journals
            .entry((corp_id, 1))
            .or_default()
            .push(CorpWalletJournalRecord {
                id: 99_201,
                date_unix: now,
                ref_type: "corporation_account_withdrawal".into(),
                description: "Ops".into(),
                amount: 10_000.0,
                balance: 5_000_000.0,
                first_party_id: Some(corp_id),
                second_party_id: Some(char_id),
                reason: Some("Ops".into()),
            });
        snap.fw_character_stats.insert(
            char_id,
            FwCharacterStatsRecord {
                character_id: char_id,
                faction_id: Some(500_001),
                enlisted_on_unix: Some(now.saturating_sub(3_600)),
                current_rank: 2,
                highest_rank: 3,
                kills: FwWindowedStat { yesterday: 1, last_week: 2, total: 5 },
                victory_points: FwWindowedStat { yesterday: 100, last_week: 250, total: 800 },
            },
        );
        snap.sov_campaign_scores.insert(
            1,
            SovCampaignScoreRecord {
                structure_id,
                attackers_score: 0.4,
                defender_score: 0.6,
            },
        );
        snap.items.insert(
            100_000_001,
            InventoryItemRecord {
                item_id: 100_000_001,
                type_id: 587,
                owner_id: char_id,
                location_id: 30_000_142,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
        snap.item_positions
            .insert(100_000_001, [1_000.0, 2_000.0, 3_000.0]);
    });

    let scopes: BTreeSet<String> = OpenApiSpec::pinned()
        .operations
        .iter()
        .flat_map(|op| op.required_scopes.iter().cloned())
        .collect();
    let code = ctx
        .db
        .authorize_pkce(
            "demo",
            "demo-secret",
            char_id,
            "evomu-cli",
            "http://localhost/callback",
            scopes.into_iter().collect(),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM",
            "S256",
            now,
        )
        .unwrap();
    let token = ctx
        .db
        .exchange_pkce_code(
            &code,
            "evomu-cli",
            "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk",
            now,
        )
        .unwrap()
        .access_token;

    (Arc::new(ctx), token, char_id, corp_id, planet_id as u32)
}

#[test]
fn esi_catalog_meta_headers_etag_error_limit_and_get_sweep() {
    let (ctx, token, char_id, corp_id, planet_id) = seed_full_esi_context();
    let router = EsiRouter::new(Arc::clone(&ctx));
    let spec = OpenApiSpec::pinned();

    // 1. Meta & Introspection endpoints + wire HTTP serialization + ETag 304
    for path in [
        "/swagger.json",
        "/openapi.json",
        "/meta",
        "/meta/compatibility-dates",
        "/meta/changelog",
        "/versions",
        "/status.json",
        "/verify",
        "/oauth/verify",
    ] {
        let resp = router.handle_request(&EsiHttpRequest::new("GET", path).with_bearer_token(&token));
        assert_eq!(resp.status_code, 200, "GET {path} failed");
        let wire = resp.to_http11_bytes();
        assert!(wire.starts_with(b"HTTP/1.1 200 OK\r\n"));
        if let Some(etag) = resp.header("etag") {
            let not_mod = router.handle_request(
                &EsiHttpRequest::new("GET", path)
                    .with_bearer_token(&token)
                    .with_header("If-None-Match", etag),
            );
            assert_eq!(not_mod.status_code, 304);
            assert!(not_mod.to_http11_bytes().starts_with(b"HTTP/1.1 304 Not Modified\r\n"));
        }
    }

    // Portrait & achievement CDN & invalid header checks
    let portrait = router.handle_request(&EsiHttpRequest::new(
        "GET",
        format!("/latest/images/characters/{char_id}/portrait?size=64"),
    ));
    assert_eq!(portrait.status_code, 200);
    assert_eq!(portrait.header("content-type"), Some("image/jpeg"));

    let mgr = &ctx.quasar_gateway.progression.achievements;
    let cdn = router.handle_request(&EsiHttpRequest::new(
        "GET",
        format!(
            "/latest/cdn/achievements/v24.1.{}/{}.json",
            mgr.sde_achievement_count, mgr.sde_achievement_hash
        ),
    ));
    assert_eq!(cdn.status_code, 200);

    let bad_date = router.handle_request(
        &EsiHttpRequest::new("GET", "/latest/status")
            .with_header("X-Compatibility-Date", "1999-01-01"),
    );
    assert_eq!(bad_date.status_code, 400);

    let bad_ds = router.handle_request(&EsiHttpRequest::new(
        "GET",
        "/latest/status?datasource=singularity",
    ));
    assert_eq!(bad_ds.status_code, 400);

    // Low error-limit router hits 420 after exhausting budget
    let strict_router = EsiRouter::with_error_limit(Arc::clone(&ctx), 1, 60);
    let r1 = strict_router.handle_request(&EsiHttpRequest::new("GET", "/latest/characters/1"));
    assert_eq!(r1.status_code, 404);
    let r2 = strict_router.handle_request(&EsiHttpRequest::new("GET", "/latest/status"));
    assert_eq!(r2.status_code, 420);

    // 2. Sweep every GET operation in OpenApiSpec with valid SDE/DB path substitutions
    let jita_celestials = ctx.sde.celestials_in_system(30_000_142);
    let star_id = jita_celestials
        .iter()
        .find(|c| c.group_id == 6)
        .map(|c| c.celestial_id)
        .unwrap_or(40_009_047);
    let moon_id = jita_celestials
        .iter()
        .find(|c| c.group_id == 8)
        .map(|c| c.celestial_id)
        .unwrap_or(40_009_049);
    let belt_id = jita_celestials
        .iter()
        .find(|c| c.group_id == 9)
        .map(|c| c.celestial_id)
        .unwrap_or(40_009_050);
    let gate_id = *ctx.sde.stargates().keys().next().unwrap_or(&50_001_248);
    let attr_id = *ctx.sde.attributes.keys().next().unwrap_or(&1);
    let eff_id = *ctx.sde.effects.keys().next().unwrap_or(&16);
    let grp_id = *ctx.sde.groups.keys().next().unwrap_or(&25);
    let cat_id = *ctx.sde.categories.keys().next().unwrap_or(&6);
    let schem_id = *ctx.sde.planet_schematics.keys().next().unwrap_or(&121);
    let mgrp_id = *ctx.sde.market_groups.keys().next().unwrap_or(&4);

    for op in &spec.operations {
        if !op.method.eq_ignore_ascii_case("GET") {
            continue;
        }
        let concrete = op
            .path_pattern
            .replace("{character_id}", &char_id.to_string())
            .replace("{corporation_id}", &corp_id.to_string())
            .replace("{alliance_id}", "99000001")
            .replace("{region_id}", "10000002")
            .replace("{constellation_id}", "20000020")
            .replace("{system_id}", "30000142")
            .replace("{origin}", "30000142")
            .replace("{destination}", "30000144")
            .replace("{station_id}", "60003760")
            .replace("{structure_id}", "1055000001")
            .replace("{type_id}", "587")
            .replace("{group_id}", &grp_id.to_string())
            .replace("{category_id}", &cat_id.to_string())
            .replace("{star_id}", &star_id.to_string())
            .replace("{planet_id}", &planet_id.to_string())
            .replace("{moon_id}", &moon_id.to_string())
            .replace("{asteroid_belt_id}", &belt_id.to_string())
            .replace("{stargate_id}", &gate_id.to_string())
            .replace("{attribute_id}", &attr_id.to_string())
            .replace("{effect_id}", &eff_id.to_string())
            .replace("{schematic_id}", &schem_id.to_string())
            .replace("{market_group_id}", &mgrp_id.to_string())
            .replace("{war_id}", "44001")
            .replace("{contract_id}", "66001")
            .replace("{killmail_id}", "55001")
            .replace("{killmail_hash}", "dummy")
            .replace("{item_id}", "100000001");
        if concrete.contains('{') {
            continue;
        }
        let query = if op.path_pattern == "/characters/{character_id}/search" {
            "?categories=solar_system,inventory_type&search=Jita"
        } else {
            ""
        };
        let target = format!("/latest{concrete}{query}");
        let resp = router.handle_request(
            &EsiHttpRequest::new("GET", &target).with_bearer_token(&token),
        );
        assert!(
            resp.status_code != 500,
            "Unexpected 500 on GET {target}: {}",
            String::from_utf8_lossy(&resp.body)
        );
    }

    // 3. Verify key JSON values on core endpoints
    let status_val = router
        .handle_request(&EsiHttpRequest::new("GET", "/latest/status"))
        .json()
        .unwrap();
    assert_eq!(status_val["vip"], json!(false));

    let insurance_val = router
        .handle_request(&EsiHttpRequest::new("GET", "/latest/insurance/prices"))
        .json()
        .unwrap();
    assert!(!insurance_val.as_array().unwrap().is_empty());

    let names_resp = router
        .handle_request(
            &EsiHttpRequest::new("POST", "/latest/universe/names")
                .with_body(serde_json::to_vec(&json!([30_000_142, 587, char_id])).unwrap()),
        )
        .json()
        .unwrap();
    assert_eq!(names_resp.as_array().unwrap().len(), 3);

    let ids_resp = router
        .handle_request(
            &EsiHttpRequest::new("POST", "/latest/universe/ids")
                .with_body(serde_json::to_vec(&json!(["Jita", "Rifter"])).unwrap()),
        )
        .json()
        .unwrap();
    assert!(!ids_resp["systems"].as_array().unwrap().is_empty());

    let route_sec = router
        .handle_request(&EsiHttpRequest::new(
            "GET",
            "/latest/route/30000142/30000144?flag=secure&avoid=30002813",
        ))
        .json()
        .unwrap();
    assert!(!route_sec.as_array().unwrap().is_empty());

    let uid = ctx.db.with_snapshot(|s| s.characters[&char_id].user_id);
    let _ = ctx
        .db
        .acquire_online_lock(uid, 999, Some(char_id), false, ctx.now_unix());
    let wp_resp = router.handle_request(
        &EsiHttpRequest::new(
            "POST",
            "/latest/ui/autopilot/waypoint?add_to_beginning=false&clear_other_waypoints=true&destination_id=30000144",
        )
        .with_bearer_token(&token),
    );
    assert_eq!(wp_resp.status_code, 204);

    // Dynamic mutated item endpoint
    ctx.db.with_mut_snapshot(|snap| {
        let mut item = InventoryItemRecord {
            item_id: 900_999_001,
            type_id: 204,
            owner_id: char_id,
            location_id: 60_003_760,
            flag: InventoryFlag::Hangar,
            quantity: 1,
            singleton: true,
            version: 1,
            dynamic_attributes: HashMap::from([(54, 25_000.0)]),
            source_mutaplasmid_id: Some(*ctx.sde.dynamic_item_attributes.keys().next().unwrap_or(&47_740)),
            insured_payout_isk: 0.0,
            is_blueprint_copy: false,
            blueprint_runs_remaining: None,
        };
        item.version = 2;
        snap.items.insert(900_999_001, item);
    });
    let dyn_resp = router
        .handle_request(&EsiHttpRequest::new("GET", "/latest/dogma/dynamic/items/204/900999001"))
        .json()
        .unwrap();
    assert_eq!(dyn_resp["created_by"], json!(char_id));
}
