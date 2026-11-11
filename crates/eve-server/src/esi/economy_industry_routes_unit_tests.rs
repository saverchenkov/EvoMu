//! Unit and mutation-killing tests for `esi::economy_industry_routes`.

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

#[test]
fn map_job_status_format_date_and_resolve_planet_type_name_cover_all_branches() {
    for (raw, expected) in [
        ("Active", "active"),
        ("active", "active"),
        ("Delivered", "delivered"),
        ("delivered", "delivered"),
        ("Cancelled", "cancelled"),
        ("cancelled", "cancelled"),
        ("Paused", "paused"),
        ("paused", "paused"),
        ("Ready", "ready"),
        ("ready", "ready"),
        ("Reverted", "reverted"),
        ("reverted", "reverted"),
        ("Unknown", "active"),
    ] {
        assert_eq!(map_job_status(raw), expected, "mismatch for {raw}");
    }

    assert_eq!(format_date_utc(0), "1970-01-01");
    assert_eq!(format_date_utc(1_700_000_000), "2023-11-14");

    let (ctx, _token, _char_id, _corp_id, planet_id) = seed_full_esi_context();
    let p_type = resolve_planet_type_name(&ctx, 30_000_142, planet_id as i64);
    assert!(
        [
            "temperate",
            "oceanic",
            "ice",
            "gas",
            "lava",
            "storm",
            "plasma",
            "shattered",
            "barren"
        ]
        .contains(&p_type)
    );
    assert_eq!(resolve_planet_type_name(&ctx, 30_000_142, 999_999_999), "barren");
}

#[test]
fn compute_industry_system_cost_indices_calculates_sqrt_clamping_and_skips_inactive() {
    let (ctx, _token, char_id, _corp_id, _planet_id) = seed_full_esi_context();
    let now = ctx.now_unix();

    ctx.db.with_mut_snapshot(|snap| {
        snap.industry_jobs.clear();
        snap.industry_jobs.insert(
            1,
            eve_db::IndustryJobRecord {
                job_id: 1,
                installer_id: char_id,
                facility_id: 60_003_760,
                activity_id: 1,
                blueprint_item_id: 100,
                blueprint_type_id: 688,
                product_type_id: 587,
                runs: 1,
                start_unix: now.saturating_sub(3600),
                duration_seconds: 1000,
                status: "Active".to_string(),
                cost: Some(5000.0),
                licensed_runs: Some(10),
                probability: Some(0.45),
                completed_unix: None,
                completed_character_id: None,
                ..Default::default()
            },
        );
        snap.industry_jobs.insert(
            2,
            eve_db::IndustryJobRecord {
                job_id: 2,
                installer_id: char_id,
                facility_id: 60_003_760,
                activity_id: 1,
                blueprint_item_id: 101,
                blueprint_type_id: 688,
                product_type_id: 587,
                runs: 1,
                start_unix: now.saturating_sub(3600),
                duration_seconds: 999_999,
                status: "Cancelled".to_string(),
                ..Default::default()
            },
        );
        snap.industry_jobs.insert(
            3,
            eve_db::IndustryJobRecord {
                job_id: 3,
                installer_id: char_id,
                facility_id: 60_003_760,
                activity_id: 8,
                blueprint_item_id: 102,
                blueprint_type_id: 688,
                product_type_id: 587,
                runs: 1,
                start_unix: now.saturating_sub(INDUSTRY_COST_INDEX_WINDOW_SECS + 100),
                duration_seconds: 50_000,
                status: "Delivered".to_string(),
                ..Default::default()
            },
        );
    });

    ctx.db.with_snapshot(|snap| {
        let rows = compute_industry_system_cost_indices(&ctx.sde, snap, now);
        let jita = rows
            .iter()
            .find(|r| r["solar_system_id"] == 30_000_142)
            .expect("Jita present");
        let indices = jita["cost_indices"].as_array().unwrap();
        let mfg = indices
            .iter()
            .find(|i| i["activity"] == "manufacturing")
            .unwrap();
        assert_eq!(mfg["cost_index"], 0.50);
        let inv = indices
            .iter()
            .find(|i| i["activity"] == "invention")
            .unwrap();
        assert_eq!(inv["cost_index"], BASELINE_SYSTEM_COST_INDEX);
    });
}

#[test]
fn format_industry_job_json_and_dispatch_industry_mining_and_pi_routes() {
    let (ctx, _token, char_id, corp_id, planet_id) = seed_full_esi_context();
    let char_str = char_id.to_string();
    let corp_str = corp_id.to_string();
    let p_char = params(&[("character_id", &char_str)]);
    let p_corp = params(&[("corporation_id", &corp_str)]);
    let empty_q = HashMap::new();

    ctx.db.with_mut_snapshot(|snap| {
        snap.industry_jobs.insert(
            777,
            eve_db::IndustryJobRecord {
                job_id: 777,
                installer_id: corp_id,
                facility_id: 60_003_760,
                activity_id: 8,
                blueprint_item_id: 500,
                blueprint_type_id: 688,
                product_type_id: 587,
                runs: 3,
                start_unix: 1_700_000_000,
                duration_seconds: 3600,
                status: "Delivered".to_string(),
                cost: Some(1234.0),
                licensed_runs: Some(5),
                probability: Some(0.5),
                completed_unix: Some(1_700_003_600),
                completed_character_id: Some(char_id),
                ..Default::default()
            },
        );
    });

    assert!(dispatch_economy_industry_route(
        &ctx,
        "POST",
        "/industry/facilities",
        &empty_q,
        &empty_q,
        None,
        1
    )
    .is_none());

    for (path, p, q) in [
        ("/industry/facilities", &empty_q, &empty_q),
        ("/industry/systems", &empty_q, &empty_q),
        (
            "/characters/{character_id}/industry/jobs",
            &p_char,
            &params(&[("include_completed", "true")]),
        ),
        ("/characters/{character_id}/mining", &p_char, &empty_q),
        (
            "/corporations/{corporation_id}/industry/jobs",
            &p_corp,
            &params(&[("include_completed", "true")]),
        ),
        (
            "/corporation/{corporation_id}/mining/extractions",
            &p_corp,
            &empty_q,
        ),
        (
            "/corporation/{corporation_id}/mining/observers",
            &p_corp,
            &empty_q,
        ),
        ("/characters/{character_id}/planets", &p_char, &empty_q),
        (
            "/corporations/{corporation_id}/customs_offices",
            &p_corp,
            &empty_q,
        ),
    ] {
        let res = dispatch_economy_industry_route(&ctx, "GET", path, p, q, None, 1).unwrap();
        assert_eq!(res.status_code, 200, "failed on {path}");
        assert!(body(&res).is_array(), "expected array on {path}");
    }

    ctx.db.with_mut_snapshot(|snap| {
        snap.observer_mining_ledger.insert(
            1_055_000_001,
            vec![eve_db::ObserverMiningRecord {
                observer_id: 1_055_000_001,
                character_id: char_id,
                recorded_corporation_id: corp_id,
                type_id: 1228,
                quantity: 500,
                last_updated_date: "2025-12-16".to_string(),
                timestamp_unix: 1_700_000_000,
            }],
        );
    });
    let p_obs = params(&[
        ("corporation_id", &corp_str),
        ("observer_id", "1055000001"),
    ]);
    let obs_res = dispatch_economy_industry_route(
        &ctx,
        "GET",
        "/corporation/{corporation_id}/mining/observers/{observer_id}",
        &p_obs,
        &empty_q,
        None,
        1,
    )
    .unwrap();
    assert_eq!(obs_res.status_code, 200);

    let p_planet = params(&[
        ("character_id", &char_str),
        ("planet_id", &planet_id.to_string()),
    ]);
    let planet_res = dispatch_economy_industry_route(
        &ctx,
        "GET",
        "/characters/{character_id}/planets/{planet_id}",
        &p_planet,
        &empty_q,
        None,
        1,
    )
    .unwrap();
    assert_eq!(planet_res.status_code, 200);
    assert!(body(&planet_res)["pins"].is_array());
    assert!(body(&planet_res)["routes"].is_array());
    assert!(body(&planet_res)["links"].is_array());

    let p_bad_planet = params(&[("character_id", &char_str), ("planet_id", "99999999")]);
    assert_eq!(
        dispatch_economy_industry_route(
            &ctx,
            "GET",
            "/characters/{character_id}/planets/{planet_id}",
            &p_bad_planet,
            &empty_q,
            None,
            1
        )
        .unwrap()
        .status_code,
        404
    );
}

#[test]
fn economy_industry_routes_errors_corp_filtering_job_meta_fallbacks_and_edge_branches() {
    use crate::services::industry_pi::{save_facility_taxes, save_job_cost_meta, IndustryJobCostMeta};
    use eve_db::{
        CustomsOfficeRecord, IndustryJobRecord, InventoryFlag, InventoryItemRecord, MiningLedgerRecord,
        MoonExtractionRecord, ObserverMiningRecord, PlanetaryColonyLayoutRecord,
        PlanetaryColonyRecord, PlanetaryLinkRecord, PlanetaryPinContentRecord, PlanetaryPinRecord,
        PlanetaryRouteRecord, UpwellStructureRecord,
    };

    let (ctx, _token, char_id, corp_id, planet_id) = seed_full_esi_context();
    let empty_q = HashMap::new();
    let empty_p = HashMap::new();
    let char_s = char_id.to_string();
    let corp_s = corp_id.to_string();
    let p_char = params(&[("character_id", &char_s)]);
    let p_corp = params(&[("corporation_id", &corp_s)]);
    let now = ctx.now_unix();
    let other_corp_id = corp_id + 55;
    let upwell_ind_id = 1_055_010_001_i64;
    let upwell_no_ind_id = 1_055_010_002_i64;
    let destroyed_obs_id = 1_055_010_003_i64;

    // 1. Path param `400` and missing entity `404` matrices across every endpoint
    let p_bad_char = params(&[("character_id", "0")]);
    let p_missing_char = params(&[("character_id", "99999999")]);
    for path in [
        "/characters/{character_id}/industry/jobs",
        "/characters/{character_id}/mining",
        "/characters/{character_id}/planets",
    ] {
        assert_eq!(
            dispatch_economy_industry_route(&ctx, "GET", path, &empty_p, &empty_q, None, 1)
                .unwrap()
                .status_code,
            400,
            "missing param {path}"
        );
        assert_eq!(
            dispatch_economy_industry_route(&ctx, "GET", path, &p_bad_char, &empty_q, None, 1)
                .unwrap()
                .status_code,
            400,
            "zero param {path}"
        );
        assert_eq!(
            dispatch_economy_industry_route(&ctx, "GET", path, &p_missing_char, &empty_q, None, 1)
                .unwrap()
                .status_code,
            404,
            "missing entity {path}"
        );
    }

    let p_bad_corp = params(&[("corporation_id", "-3")]);
    let p_missing_corp = params(&[("corporation_id", "99999999")]);
    for path in [
        "/corporations/{corporation_id}/industry/jobs",
        "/corporation/{corporation_id}/mining/extractions",
        "/corporation/{corporation_id}/mining/observers",
        "/corporations/{corporation_id}/customs_offices",
    ] {
        assert_eq!(
            dispatch_economy_industry_route(&ctx, "GET", path, &p_bad_corp, &empty_q, None, 1)
                .unwrap()
                .status_code,
            400
        );
        assert_eq!(
            dispatch_economy_industry_route(&ctx, "GET", path, &p_missing_corp, &empty_q, None, 1)
                .unwrap()
                .status_code,
            404
        );
    }

    // Observer & planet multi-param error cases
    assert_eq!(
        dispatch_economy_industry_route(
            &ctx,
            "GET",
            "/corporation/{corporation_id}/mining/observers/{observer_id}",
            &params(&[("corporation_id", &corp_s), ("observer_id", "bad")]),
            &empty_q,
            None,
            1
        )
        .unwrap()
        .status_code,
        400
    );
    assert_eq!(
        dispatch_economy_industry_route(
            &ctx,
            "GET",
            "/corporation/{corporation_id}/mining/observers/{observer_id}",
            &params(&[("corporation_id", &corp_s), ("observer_id", "77777777")]),
            &empty_q,
            None,
            1
        )
        .unwrap()
        .status_code,
        404
    );
    assert_eq!(
        dispatch_economy_industry_route(
            &ctx,
            "GET",
            "/characters/{character_id}/planets/{planet_id}",
            &params(&[("character_id", "99999999"), ("planet_id", &planet_id.to_string())]),
            &empty_q,
            None,
            1
        )
        .unwrap()
        .status_code,
        404
    );
    assert_eq!(
        dispatch_economy_industry_route(
            &ctx,
            "GET",
            "/characters/{character_id}/planets/{planet_id}",
            &params(&[("character_id", &char_s), ("planet_id", "0")]),
            &empty_q,
            None,
            1
        )
        .unwrap()
        .status_code,
        400
    );

    // 2. Seed rich facility, industry job, mining, extraction, observer, PI, and POCO edge cases
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            upwell_ind_id,
            UpwellStructureRecord {
                structure_id: upwell_ind_id,
                type_id: 35_825,
                owner_corp_id: 1_000_009,
                solar_system_id: 30_000_142,
                name: "Raitaru Factory".into(),
                position: [0.0, 0.0, 0.0],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 500,
                ..Default::default()
            },
        );
        // Online Standup Manufacturing Plant I (35878) -> activity_id 1
        snap.items.insert(
            upwell_ind_id + 1,
            InventoryItemRecord {
                item_id: upwell_ind_id + 1,
                type_id: 35_878,
                owner_id: 1_000_009,
                location_id: upwell_ind_id,
                flag: InventoryFlag::from_u16(164),
                quantity: 1,
                singleton: true,
                version: 1,
                dynamic_attributes: HashMap::from([(2, 1.0)]),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
        snap.module_online_states.insert(upwell_ind_id + 1, true);
        save_facility_taxes(snap, upwell_ind_id, &HashMap::from([(1_u8, 0.0375)]));

        // Upwell structure without any industry module (skipped from `/industry/facilities`)
        snap.structures.insert(
            upwell_no_ind_id,
            UpwellStructureRecord {
                structure_id: upwell_no_ind_id,
                type_id: 35_832,
                owner_corp_id: corp_id,
                solar_system_id: 30_000_142,
                name: "Bare Astrahus".into(),
                position: [1_000.0, 0.0, 0.0],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 500,
                ..Default::default()
            },
        );

        // Corp-owned refinery for moon extraction tests (unlike public NPC-owned `upwell_ind_id`)
        let upwell_corp_moon_id = upwell_ind_id + 10;
        snap.structures.insert(
            upwell_corp_moon_id,
            UpwellStructureRecord {
                structure_id: upwell_corp_moon_id,
                type_id: 35_835,
                owner_corp_id: corp_id,
                solar_system_id: 30_000_142,
                name: "Live Athanor".into(),
                position: [1_500.0, 0.0, 0.0],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 500,
                ..Default::default()
            },
        );

        // Destroyed observer owned by corp + entries recorded by ANOTHER corp
        snap.structures.insert(
            destroyed_obs_id,
            UpwellStructureRecord {
                structure_id: destroyed_obs_id,
                type_id: 35_835,
                owner_corp_id: corp_id,
                solar_system_id: 30_000_142,
                name: "Dead Athanor".into(),
                position: [2_000.0, 0.0, 0.0],
                state: "Destroyed".into(),
                ..Default::default()
            },
        );
        snap.observer_mining_ledger.insert(
            destroyed_obs_id,
            vec![ObserverMiningRecord {
                observer_id: destroyed_obs_id,
                character_id: char_id,
                recorded_corporation_id: other_corp_id,
                type_id: 1228,
                quantity: 100,
                last_updated_date: String::new(),
                timestamp_unix: 1_700_000_000,
            }],
        );
        // Live observer with empty `last_updated_date` fallback to `format_date_utc` + zero-entry observer
        snap.observer_mining_ledger.insert(upwell_ind_id + 50, Vec::new());
        snap.observer_mining_ledger.insert(
            upwell_ind_id,
            vec![
                ObserverMiningRecord {
                    observer_id: upwell_ind_id,
                    character_id: char_id,
                    recorded_corporation_id: corp_id,
                    type_id: 1228,
                    quantity: 200,
                    last_updated_date: String::new(),
                    timestamp_unix: 1_700_000_000,
                },
                ObserverMiningRecord {
                    observer_id: upwell_ind_id,
                    character_id: char_id + 1,
                    recorded_corporation_id: corp_id,
                    type_id: 1230,
                    quantity: 150,
                    last_updated_date: "2024-01-02".into(),
                    timestamp_unix: 1_704_153_600,
                },
            ],
        );

        // Industry jobs:
        // - Job 901: personal Copying job (activity_id 5) paused, cancelled with meta fallback locations/cost
        // - Job 902: installer_id == char_id BUT `use_corp == true` -> excluded from char jobs, included in corp jobs!
        snap.industry_jobs.clear();
        snap.industry_jobs.insert(
            901,
            IndustryJobRecord {
                job_id: 901,
                installer_id: char_id,
                facility_id: 60_003_760,
                activity_id: 5,
                blueprint_item_id: 100_000_050,
                blueprint_type_id: 688,
                product_type_id: 688,
                runs: 4,
                start_unix: now.saturating_sub(1_200),
                duration_seconds: 3_600,
                status: "Cancelled".into(),
                cost: None,
                licensed_runs: None,
                probability: None,
                completed_unix: None,
                completed_character_id: None,
                ..Default::default()
            },
        );
        save_job_cost_meta(
            snap,
            901,
            &IndustryJobCostMeta {
                total_cost_isk: 7_777.0,
                paused_at_unix: Some(now.saturating_sub(600)),
                blueprint_location_id: 60_003_761,
                output_location_id: 60_003_762,
                completed_unix: Some(now.saturating_sub(100)),
                completed_character_id: Some(char_id),
                ..Default::default()
            },
        );
        snap.industry_jobs.insert(
            902,
            IndustryJobRecord {
                job_id: 902,
                installer_id: char_id,
                facility_id: 60_003_760,
                activity_id: 8,
                blueprint_item_id: 100_000_051,
                blueprint_type_id: 688,
                product_type_id: 587,
                runs: 1,
                start_unix: now.saturating_sub(300),
                duration_seconds: 1_200,
                status: "Active".into(),
                ..Default::default()
            },
        );
        save_job_cost_meta(
            snap,
            902,
            &IndustryJobCostMeta {
                use_corp: true,
                owner_id: corp_id,
                location_id: 60_003_765,
                invented_runs: 10,
                inv_prob: 0.35,
                ..Default::default()
            },
        );

        // Mining ledger: empty date fallback + aggregation + skip quantity <= 0 and > 30 days old
        snap.mining_ledger.clear();
        snap.mining_ledger.push(MiningLedgerRecord {
            character_id: char_id,
            solar_system_id: 30_000_142,
            type_id: 1230,
            quantity: 250,
            date: String::new(),
            timestamp_unix: 1_700_000_000.max(now.saturating_sub(3600)),
        });
        snap.mining_ledger.push(MiningLedgerRecord {
            character_id: char_id,
            solar_system_id: 30_000_142,
            type_id: 1230,
            quantity: 150,
            date: format_date_utc(1_700_000_000.max(now.saturating_sub(3600))),
            timestamp_unix: 1_700_000_000.max(now.saturating_sub(3600)),
        });
        snap.mining_ledger.push(MiningLedgerRecord {
            character_id: char_id,
            solar_system_id: 30_000_142,
            type_id: 1230,
            quantity: 0,
            date: "2026-01-01".into(),
            timestamp_unix: now.saturating_sub(60),
        });
        snap.mining_ledger.push(MiningLedgerRecord {
            character_id: char_id,
            solar_system_id: 30_000_142,
            type_id: 1230,
            quantity: 999,
            date: "2020-01-01".into(),
            timestamp_unix: now.saturating_sub(31 * 86_400),
        });

        // Moon extractions: one Active (corp) + one Cancelled (excluded)
        let upwell_corp_moon_id = upwell_ind_id + 10;
        snap.moon_extractions.clear();
        snap.moon_extractions.insert(
            upwell_corp_moon_id,
            MoonExtractionRecord {
                structure_id: upwell_corp_moon_id,
                moon_id: 40_009_082,
                extraction_start_unix: now.saturating_sub(1_000),
                chunk_arrival_unix: now + 5_000,
                natural_decay_unix: now + 8_600,
                status: "Started".into(),
                auto_fracture: false,
                event_id: 1,
                moon_ores: Vec::new(),
                spawned_asteroid_ids: Vec::new(),
            },
        );
        snap.moon_extractions.insert(
            upwell_no_ind_id,
            MoonExtractionRecord {
                structure_id: upwell_no_ind_id,
                moon_id: 40_009_083,
                extraction_start_unix: now.saturating_sub(2_000),
                chunk_arrival_unix: now + 4_000,
                natural_decay_unix: now + 7_600,
                status: "Cancelled".into(),
                auto_fracture: false,
                event_id: 2,
                moon_ores: Vec::new(),
                spawned_asteroid_ids: Vec::new(),
            },
        );

        // PI colony with populated pin optional fields + second colony without layout
        let alt_planet = planet_id as i64 + 1;
        snap.planetary_colonies.insert(
            (char_id, alt_planet),
            PlanetaryColonyRecord {
                planet_id: alt_planet,
                owner_id: char_id,
                solar_system_id: 30_000_142,
                command_center_level: 3,
                pins_count: 1,
                extracted_commodities: HashMap::new(),
                last_extracted_unix: now,
            },
        );
        snap.planetary_colony_layouts.insert(
            (char_id, planet_id as i64),
            PlanetaryColonyLayoutRecord {
                pins: vec![PlanetaryPinRecord {
                    pin_id: 77_001,
                    type_id: 2470,
                    latitude: 0.5,
                    longitude: 1.2,
                    schematic_id: Some(121),
                    install_time_unix: Some(1_700_000_000),
                    expiry_time_unix: Some(1_700_086_400),
                    last_cycle_start_unix: Some(1_700_001_000),
                    contents: vec![PlanetaryPinContentRecord { type_id: 2268, amount: 500 }],
                }],
                links: vec![PlanetaryLinkRecord {
                    source_pin_id: 77_001,
                    destination_pin_id: 77_002,
                    link_level: 2,
                }],
                routes: vec![PlanetaryRouteRecord {
                    route_id: 88_001,
                    source_pin_id: 77_001,
                    destination_pin_id: 77_002,
                    content_type_id: 2268,
                    quantity: 40,
                    waypoints: vec![77_003],
                }],
            },
        );
        snap.planetary_colony_layouts.remove(&(char_id, alt_planet));

        // Customs offices with all optional standing/tax rates as None vs Some
        snap.corp_customs_offices.insert(
            corp_id,
            vec![
                CustomsOfficeRecord {
                    office_id: 90_002,
                    system_id: 30_000_142,
                    reinforce_exit_start: 18,
                    reinforce_exit_end: 20,
                    allow_alliance_access: false,
                    allow_access_with_standings: false,
                    standing_level: None,
                    corporation_tax_rate: None,
                    alliance_tax_rate: None,
                    excellent_standing_tax_rate: None,
                    good_standing_tax_rate: None,
                    neutral_standing_tax_rate: None,
                    bad_standing_tax_rate: None,
                    terrible_standing_tax_rate: None,
                },
                CustomsOfficeRecord {
                    office_id: 90_001,
                    system_id: 30_000_142,
                    reinforce_exit_start: 0,
                    reinforce_exit_end: 2,
                    allow_alliance_access: true,
                    allow_access_with_standings: true,
                    standing_level: Some("good".into()),
                    corporation_tax_rate: Some(0.05),
                    alliance_tax_rate: Some(0.08),
                    excellent_standing_tax_rate: Some(0.02),
                    good_standing_tax_rate: Some(0.04),
                    neutral_standing_tax_rate: Some(0.10),
                    bad_standing_tax_rate: Some(0.15),
                    terrible_standing_tax_rate: Some(0.25),
                },
            ],
        );
    });

    // Verify /industry/facilities includes Upwell structure with custom tax 0.0375 and excludes bare structure
    let fac_res = dispatch_economy_industry_route(&ctx, "GET", "/industry/facilities", &empty_p, &empty_q, None, 1).unwrap();
    let facs = body(&fac_res).as_array().unwrap();
    let ind_fac = facs.iter().find(|f| f["facility_id"] == upwell_ind_id).expect("Raitaru emitted");
    assert_eq!(ind_fac["tax"], 0.0375);
    assert!(!facs.iter().any(|f| f["facility_id"] == upwell_no_ind_id));

    // Verify character jobs exclude job 902 (`use_corp == true`), include job 901, with `include_completed=false` excluding cancelled 901
    let cjobs_all = body(&dispatch_economy_industry_route(
        &ctx,
        "GET",
        "/characters/{character_id}/industry/jobs",
        &p_char,
        &empty_q,
        None,
        1,
    )
    .unwrap())
    .as_array()
    .unwrap()
    .clone();
    assert_eq!(cjobs_all.len(), 1);
    assert_eq!(cjobs_all[0]["job_id"], 901);
    assert_eq!(cjobs_all[0]["status"], "cancelled");
    assert_eq!(cjobs_all[0]["licensed_runs"], 4);
    assert_eq!(cjobs_all[0]["cost"], 7_777.0);
    assert_eq!(cjobs_all[0]["blueprint_location_id"], 60_003_761);
    assert_eq!(cjobs_all[0]["output_location_id"], 60_003_762);
    assert_eq!(cjobs_all[0]["successful_runs"], 0);
    assert!(cjobs_all[0].get("pause_date").is_some());
    assert!(cjobs_all[0].get("completed_date").is_some());

    let cjobs_active = dispatch_economy_industry_route(
        &ctx,
        "GET",
        "/characters/{character_id}/industry/jobs",
        &p_char,
        &params(&[("include_completed", "false")]),
        None,
        1,
    )
    .unwrap();
    assert_eq!(body(&cjobs_active), &json!([]));

    // Verify corp jobs include job 902 with `location_id == 60_003_765` and invented metadata
    let corp_jobs = dispatch_economy_industry_route(
        &ctx,
        "GET",
        "/corporations/{corporation_id}/industry/jobs",
        &p_corp,
        &params(&[("include_completed", "false")]),
        None,
        1,
    )
    .unwrap();
    let cj = &body(&corp_jobs)[0];
    assert_eq!(cj["job_id"], 902);
    assert_eq!(cj["location_id"], 60_003_765);
    assert_eq!(cj["licensed_runs"], 10);
    assert_eq!(cj["probability"], 0.35);

    // Mining ledger aggregation + out-of-range pagination -> 404
    let mining = dispatch_economy_industry_route(&ctx, "GET", "/characters/{character_id}/mining", &p_char, &empty_q, None, 1).unwrap();
    let mining_rows = body(&mining).as_array().unwrap();
    assert_eq!(mining_rows.len(), 1);
    assert_eq!(mining_rows[0]["quantity"], 400);
    assert_eq!(
        dispatch_economy_industry_route(&ctx, "GET", "/characters/{character_id}/mining", &p_char, &empty_q, None, 99)
            .unwrap()
            .status_code,
        404
    );

    // Extractions exclude Cancelled
    let extr = dispatch_economy_industry_route(&ctx, "GET", "/corporation/{corporation_id}/mining/extractions", &p_corp, &empty_q, None, 1).unwrap();
    assert_eq!(body(&extr).as_array().unwrap().len(), 1);
    assert_eq!(body(&extr)[0]["structure_id"], upwell_ind_id + 10);

    // Observers: empty skipped, destroyed-without-matching-corp -> 404, empty `last_updated_date` uses UTC fallback
    assert_eq!(
        dispatch_economy_industry_route(
            &ctx,
            "GET",
            "/corporation/{corporation_id}/mining/observers/{observer_id}",
            &params(&[("corporation_id", &corp_s), ("observer_id", &destroyed_obs_id.to_string())]),
            &empty_q,
            None,
            1
        )
        .unwrap()
        .status_code,
        404
    );
    let obs_list = dispatch_economy_industry_route(&ctx, "GET", "/corporation/{corporation_id}/mining/observers", &p_corp, &empty_q, None, 1).unwrap();
    assert_eq!(body(&obs_list).as_array().unwrap().len(), 1);
    let obs_rows = dispatch_economy_industry_route(
        &ctx,
        "GET",
        "/corporation/{corporation_id}/mining/observers/{observer_id}",
        &params(&[("corporation_id", &corp_s), ("observer_id", &upwell_ind_id.to_string())]),
        &empty_q,
        None,
        1
    )
    .unwrap();
    assert_eq!(body(&obs_rows)[0]["last_updated"], "2023-11-14");

    // Planets: populated optional pin/route fields vs empty default layout + resolve_planet_type_name variants across SDE
    let planet_full = dispatch_economy_industry_route(
        &ctx,
        "GET",
        "/characters/{character_id}/planets/{planet_id}",
        &params(&[("character_id", &char_s), ("planet_id", &planet_id.to_string())]),
        &empty_q,
        None,
        1,
    )
    .unwrap();
    assert_eq!(body(&planet_full)["pins"][0]["schematic_id"], 121);
    assert_eq!(body(&planet_full)["pins"][0]["contents"][0]["amount"], 500);
    assert_eq!(body(&planet_full)["routes"][0]["waypoints"], json!([77_003]));

    let planet_empty = dispatch_economy_industry_route(
        &ctx,
        "GET",
        "/characters/{character_id}/planets/{planet_id}",
        &params(&[("character_id", &char_s), ("planet_id", &(planet_id + 1).to_string())]),
        &empty_q,
        None,
        1,
    )
    .unwrap();
    assert_eq!(body(&planet_empty), &json!({ "pins": [], "links": [], "routes": [] }));

    // Exercise resolve_planet_type_name across real SDE systems to hit planet-type keywords
    let mut seen_types = std::collections::HashSet::new();
    for &sid in ctx.sde.solar_systems.keys().take(40) {
        for c in ctx.sde.celestials_in_system(sid) {
            if c.group_id == 7 {
                seen_types.insert(resolve_planet_type_name(&ctx, sid, c.celestial_id));
            }
        }
        if seen_types.len() >= 8 {
            break;
        }
    }
    assert!(seen_types.len() >= 4, "expected varied SDE planet types, got {seen_types:?}");

    // Customs offices sorted by office_id, optional rates omitted when None vs emitted when Some
    let pocos = dispatch_economy_industry_route(&ctx, "GET", "/corporations/{corporation_id}/customs_offices", &p_corp, &empty_q, None, 1).unwrap();
    let poco_arr = body(&pocos).as_array().unwrap();
    assert_eq!(poco_arr.len(), 2);
    assert_eq!(poco_arr[0]["office_id"], 90_001);
    assert_eq!(poco_arr[0]["standing_level"], "good");
    assert_eq!(poco_arr[1]["office_id"], 90_002);
    assert!(poco_arr[1].get("standing_level").is_none());
    assert!(poco_arr[1].get("corporation_tax_rate").is_none());
}
