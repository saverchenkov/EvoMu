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
