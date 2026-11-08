//! Deep branch-coverage and mutation-killing unit tests for `esi::economy_industry_routes`.

use super::*;
use crate::esi::full_endpoint_catalog_tests::seed_full_esi_context;
use crate::services::industry_pi::industry_facilities_bps::{
    save_facility_taxes, IndustryJobCostMeta, NPC_STATION_INDUSTRY_TAX_RATE,
};
use crate::services::industry_pi::save_job_cost_meta;
use eve_db::{
    CustomsOfficeRecord, IndustryJobRecord, InventoryFlag, InventoryItemRecord, MiningLedgerRecord,
    MoonExtractionRecord, ObserverMiningRecord, PlanetaryColonyLayoutRecord, PlanetaryColonyRecord,
    PlanetaryLinkRecord, PlanetaryPinContentRecord, PlanetaryPinRecord, PlanetaryRouteRecord,
    UpwellStructureRecord,
};
use eve_sde::{InvType, StationInfo};
use serde_json::json;
use std::path::PathBuf;
use std::sync::Arc;

struct TempCelestialDb(PathBuf);
impl Drop for TempCelestialDb {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn params(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs.iter().map(|(k, v)| ((*k).to_string(), (*v).to_string())).collect()
}

fn body(res: &RouteExecutionResult) -> &Value {
    res.body.as_ref().expect("response body")
}

fn mk_job(id: i64, installer: i64, fac: i64, act: u8, bp_item: i64, bp_type: u32, prod: u32, runs: u32, start: u64, dur: u64, status: &str) -> IndustryJobRecord {
    IndustryJobRecord {
        job_id: id,
        installer_id: installer,
        facility_id: fac,
        activity_id: act,
        blueprint_item_id: bp_item,
        blueprint_type_id: bp_type,
        product_type_id: prod,
        runs,
        start_unix: start,
        duration_seconds: dur,
        status: status.into(),
        ..Default::default()
    }
}

fn make_celestial_db(tag: &str, rows: &[(i64, u32, u32, u32, &str, f64)]) -> TempCelestialDb {
    let path = std::env::temp_dir().join(format!("evomu_cov_econ_ind_{tag}_{}_{}.sqlite", std::process::id(), rows.len()));
    let _ = std::fs::remove_file(&path);
    let conn = rusqlite::Connection::open(&path).expect("open temp celestial sqlite");
    conn.execute_batch(
        "CREATE TABLE sde_celestials (
            celestial_id INTEGER PRIMARY KEY, system_id INTEGER NOT NULL, type_id INTEGER NOT NULL,
            group_id INTEGER NOT NULL, name TEXT NOT NULL, radius REAL NOT NULL,
            x REAL NOT NULL, y REAL NOT NULL, z REAL NOT NULL,
            orbit_id INTEGER, orbit_index INTEGER, celestial_index INTEGER
        );",
    )
    .expect("create sde_celestials");
    for &(cid, sys, tid, gid, name, radius) in rows {
        conn.execute(
            "INSERT INTO sde_celestials VALUES (?1, ?2, ?3, ?4, ?5, ?6, 0.0, 0.0, 0.0, NULL, NULL, NULL)",
            rusqlite::params![cid, i64::from(sys), i64::from(tid), i64::from(gid), name, radius],
        )
        .expect("insert sde_celestials row");
    }
    TempCelestialDb(path)
}

#[test]
fn system_cost_indices_all_activities_window_boundaries_structures_and_clamps() {
    let (base_ctx, _tok, char_id, corp_id, _pid) = seed_full_esi_context();
    let now = 10_000_000_u64;
    let cutoff = now - INDUSTRY_COST_INDEX_WINDOW_SECS;

    let sys_a = 30_000_142_u32;
    let sys_b = base_ctx.sde.solar_systems.keys().copied().find(|&s| s != sys_a).expect("second system");
    let st_b = base_ctx.sde.stations.values().find(|st| st.solar_system_id == sys_b).map(|st| st.station_id).unwrap_or(1_066_000_002);
    let upwell_b = 1_066_000_099_i64;

    base_ctx.db.with_mut_snapshot(|snap| {
        snap.industry_jobs.clear();
        snap.structures.insert(
            upwell_b,
            UpwellStructureRecord {
                structure_id: upwell_b,
                type_id: 35_825,
                owner_corp_id: corp_id,
                solar_system_id: sys_b,
                name: "Raitaru B".into(),
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 200,
                ..Default::default()
            },
        );
        let j = |id, fac, act, start, dur, st| mk_job(id, char_id, fac, act, 1_000 + id, 688, 587, 1, start, dur, st);
        // Skipped jobs: Cancelled, Reverted, before cutoff (cutoff - 1), unknown facility, unsupported activity, zero duration
        for (id, fac, act, start, dur, st) in [
            (1, 60_003_760, 1, now - 10, 5_000, "Cancelled"),
            (2, 60_003_760, 1, now - 10, 5_000, "reverted"),
            (3, 60_003_760, 1, cutoff - 1, 5_000, "Delivered"),
            (4, 999_999_999, 1, now - 10, 5_000, "Active"),
            (5, 60_003_760, 2, now - 10, 5_000, "Active"),
            (6, 60_003_760, 1, now - 10, 0, "Active"),
            (10, 60_003_760, 1, cutoff, 900, "Delivered"),
            (11, upwell_b, 1, now - 50, 9_100, "Active"),
            (12, 60_003_760, 3, 0, 400, "Active"),
            (13, 60_003_760, 4, now - 5, 250, "Active"),
            (14, 60_003_760, 5, now - 5, 300, "Active"),
            (15, 60_003_760, 8, now - 5, 1, "Active"),
            (16, upwell_b, 8, now - 5, 9_999_999, "Active"),
            (17, 60_003_760, 9, now - 5, 100, "Active"),
            (18, st_b.max(upwell_b), 11, now - 5, 300, "Active"),
        ] {
            snap.industry_jobs.insert(id, j(id, fac, act, start, dur, st));
        }
    });

    base_ctx.db.with_snapshot(|snap| {
        let out = compute_industry_system_cost_indices(&base_ctx.sde, snap, now);
        let row_a = out.iter().find(|r| r["solar_system_id"] == i64::from(sys_a)).unwrap();
        let row_b = out.iter().find(|r| r["solar_system_id"] == i64::from(sys_b)).unwrap();
        let idx_of = |row: &Value, act: &str| -> f64 {
            row["cost_indices"].as_array().unwrap().iter().find(|e| e["activity"] == act).unwrap()["cost_index"].as_f64().unwrap()
        };
        assert!((idx_of(row_a, "manufacturing") - 0.30).abs() < 1e-9);
        assert_eq!(idx_of(row_b, "manufacturing"), 0.50);
        assert_eq!(idx_of(row_a, "researching_time_efficiency"), 0.50);
        assert_eq!(idx_of(row_b, "researching_time_efficiency"), BASELINE_SYSTEM_COST_INDEX);
        assert_eq!(idx_of(row_a, "researching_material_efficiency"), 0.50);
        assert_eq!(idx_of(row_a, "copying"), 0.50);
        assert_eq!(idx_of(row_a, "invention"), 0.001);
        assert_eq!(idx_of(row_b, "invention"), 0.50);
        assert!((idx_of(row_a, "reaction") - 0.50).abs() < 1e-9);
    });
}

#[test]
fn resolve_planet_types_and_pi_colonies_layouts_and_customs_offices() {
    let (base_ctx, _tok, char_id, corp_id, _pid) = seed_full_esi_context();
    let planet_specs = [
        (40_900_001_i64, 990_001_u32, 7_u32, "Planet (Temperate)", "temperate"),
        (40_900_002, 990_002, 7, "Planet (Oceanic)", "oceanic"),
        (40_900_003, 990_003, 7, "Planet (Ice)", "ice"),
        (40_900_004, 990_004, 7, "Planet (Gas)", "gas"),
        (40_900_005, 990_005, 7, "Planet (Lava)", "lava"),
        (40_900_006, 990_006, 7, "Planet (Storm)", "storm"),
        (40_900_007, 990_007, 7, "Planet (Plasma)", "plasma"),
        (40_900_008, 990_008, 7, "Planet (Shattered)", "shattered"),
        (40_900_009, 990_009, 7, "Planet (Barren)", "barren"),
        (40_900_010, 990_001, 8, "Moon Not Planet", "barren"),
        (40_900_011, 990_999, 7, "Missing Type Planet", "barren"),
    ];
    let db_rows: Vec<_> = planet_specs.iter().map(|&(cid, tid, gid, nm, _)| (cid, 30_000_142_u32, tid, gid, nm, 6_000_000.0)).collect();
    let tmp_db = make_celestial_db("planets", &db_rows);

    let mut sde = (*base_ctx.sde).clone();
    sde.db_path = Some(tmp_db.0.clone());
    let tpl: InvType = sde.types.values().next().cloned().expect("seed type");
    for &(_, tid, _, nm, _) in &planet_specs[..9] {
        let mut t = tpl.clone();
        t.type_id = tid;
        t.group_id = 7;
        t.name = nm.to_string();
        sde.types.insert(tid, t);
    }
    sde.types.remove(&990_999);

    let ctx = ServerContext::with_sde_and_repository(Arc::new(sde), Arc::clone(&base_ctx.db));
    for &(cid, _, _, _, expected) in &planet_specs {
        assert_eq!(resolve_planet_type_name(&ctx, 30_000_142, cid), expected);
    }

    ctx.db.with_mut_snapshot(|snap| {
        snap.planetary_colonies.clear();
        for (pid, lvl, pins, ts) in [(40_900_002, 4, 2, 1_700_000_000), (40_900_005, 2, 1, 1_700_000_500)] {
            snap.planetary_colonies.insert(
                (char_id, pid),
                PlanetaryColonyRecord {
                    planet_id: pid,
                    owner_id: char_id,
                    solar_system_id: 30_000_142,
                    command_center_level: lvl,
                    pins_count: pins,
                    extracted_commodities: HashMap::new(),
                    last_extracted_unix: ts,
                },
            );
        }
        snap.planetary_colony_layouts.insert(
            (char_id, 40_900_002),
            PlanetaryColonyLayoutRecord {
                pins: vec![
                    PlanetaryPinRecord {
                        pin_id: 101,
                        type_id: 2254,
                        latitude: 0.5,
                        longitude: 1.2,
                        schematic_id: Some(131),
                        install_time_unix: Some(1_700_000_000),
                        expiry_time_unix: Some(1_700_086_400),
                        last_cycle_start_unix: Some(1_700_003_600),
                        contents: vec![PlanetaryPinContentRecord { type_id: 2389, amount: 500 }],
                    },
                    PlanetaryPinRecord {
                        pin_id: 102,
                        type_id: 2256,
                        latitude: 0.1,
                        longitude: 0.2,
                        schematic_id: None,
                        install_time_unix: None,
                        expiry_time_unix: None,
                        last_cycle_start_unix: None,
                        contents: vec![],
                    },
                ],
                links: vec![PlanetaryLinkRecord { source_pin_id: 101, destination_pin_id: 102, link_level: 3 }],
                routes: vec![
                    PlanetaryRouteRecord { route_id: 1, source_pin_id: 101, destination_pin_id: 102, content_type_id: 2389, quantity: 20, waypoints: vec![103, 104] },
                    PlanetaryRouteRecord { route_id: 2, source_pin_id: 102, destination_pin_id: 101, content_type_id: 2390, quantity: 10, waypoints: vec![] },
                ],
            },
        );
        snap.corp_customs_offices.insert(
            corp_id,
            vec![
                CustomsOfficeRecord {
                    office_id: 200,
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
                    office_id: 100,
                    system_id: 30_000_142,
                    reinforce_exit_start: 10,
                    reinforce_exit_end: 12,
                    allow_alliance_access: true,
                    allow_access_with_standings: true,
                    standing_level: Some("good".into()),
                    corporation_tax_rate: Some(0.01),
                    alliance_tax_rate: Some(0.02),
                    excellent_standing_tax_rate: Some(0.03),
                    good_standing_tax_rate: Some(0.04),
                    neutral_standing_tax_rate: Some(0.05),
                    bad_standing_tax_rate: Some(0.10),
                    terrible_standing_tax_rate: Some(0.15),
                },
            ],
        );
    });

    let empty = HashMap::new();
    let char_s = char_id.to_string();
    let corp_s = corp_id.to_string();

    assert_eq!(dispatch_economy_industry_route(&ctx, "GET", "/characters/{character_id}/planets", &empty, &empty, None, 1).unwrap().status_code, 400);
    assert_eq!(dispatch_economy_industry_route(&ctx, "GET", "/characters/{character_id}/planets", &params(&[("character_id", "99999999")]), &empty, None, 1).unwrap().status_code, 404);
    assert_eq!(dispatch_economy_industry_route(&ctx, "GET", "/characters/{character_id}/planets/{planet_id}", &empty, &empty, None, 1).unwrap().status_code, 400);
    assert_eq!(dispatch_economy_industry_route(&ctx, "GET", "/characters/{character_id}/planets/{planet_id}", &params(&[("character_id", &char_s)]), &empty, None, 1).unwrap().status_code, 400);
    assert_eq!(dispatch_economy_industry_route(&ctx, "GET", "/characters/{character_id}/planets/{planet_id}", &params(&[("character_id", "99999999"), ("planet_id", "40900002")]), &empty, None, 1).unwrap().status_code, 404);

    let cols = dispatch_economy_industry_route(&ctx, "GET", "/characters/{character_id}/planets", &params(&[("character_id", &char_s)]), &empty, None, 1).unwrap();
    let col_arr = body(&cols).as_array().unwrap();
    assert_eq!(col_arr.len(), 2);
    assert_eq!(col_arr[0]["planet_type"], "oceanic");
    assert_eq!(col_arr[0]["upgrade_level"], 4);
    assert_eq!(col_arr[1]["planet_type"], "lava");

    let lay = dispatch_economy_industry_route(&ctx, "GET", "/characters/{character_id}/planets/{planet_id}", &params(&[("character_id", &char_s), ("planet_id", "40900002")]), &empty, None, 1).unwrap();
    let pins = body(&lay)["pins"].as_array().unwrap();
    assert_eq!(pins[0]["schematic_id"], 131);
    assert_eq!(pins[0]["contents"][0]["amount"], 500);
    assert!(pins[0]["install_time"].is_string() && pins[0]["expiry_time"].is_string() && pins[0]["last_cycle_start"].is_string());
    assert!(pins[1].get("schematic_id").is_none() && pins[1].get("contents").is_none());
    let routes = body(&lay)["routes"].as_array().unwrap();
    assert_eq!(routes[0]["waypoints"], json!([103, 104]));
    assert!(routes[1].get("waypoints").is_none());

    let lay_empty = dispatch_economy_industry_route(&ctx, "GET", "/characters/{character_id}/planets/{planet_id}", &params(&[("character_id", &char_s), ("planet_id", "40900005")]), &empty, None, 1).unwrap();
    assert_eq!(body(&lay_empty)["pins"], json!([]));

    assert_eq!(dispatch_economy_industry_route(&ctx, "GET", "/corporations/{corporation_id}/customs_offices", &empty, &empty, None, 1).unwrap().status_code, 400);
    assert_eq!(dispatch_economy_industry_route(&ctx, "GET", "/corporations/{corporation_id}/customs_offices", &params(&[("corporation_id", "99999999")]), &empty, None, 1).unwrap().status_code, 404);
    assert_eq!(dispatch_economy_industry_route(&ctx, "GET", "/corporations/{corporation_id}/customs_offices", &params(&[("corporation_id", &corp_s)]), &empty, None, 0).unwrap().status_code, 400);
    let pocos = dispatch_economy_industry_route(&ctx, "GET", "/corporations/{corporation_id}/customs_offices", &params(&[("corporation_id", &corp_s)]), &empty, None, 1).unwrap();
    let poco_arr = body(&pocos).as_array().unwrap();
    assert_eq!(poco_arr.len(), 2);
    assert_eq!(poco_arr[0]["office_id"], 100);
    assert_eq!(poco_arr[0]["standing_level"], "good");
    assert_eq!(poco_arr[0]["terrible_standing_tax_rate"], 0.15);
    assert!(poco_arr[1].get("standing_level").is_none() && poco_arr[1].get("corporation_tax_rate").is_none());
}

#[test]
fn facilities_jobs_format_mining_extractions_and_observers_edge_cases() {
    let (base_ctx, _tok, char_id, corp_id, _pid) = seed_full_esi_context();
    let mut sde = (*base_ctx.sde).clone();
    let non_ind_st_id = 60_999_888_i64;
    let mut st_tpl: StationInfo = sde.stations.values().next().cloned().expect("station");
    st_tpl.station_id = non_ind_st_id;
    st_tpl.operation_id = 250;
    sde.station_operations.remove(&250);
    sde.stations.insert(non_ind_st_id, st_tpl);
    let ctx = ServerContext::with_sde_and_repository(Arc::new(sde), Arc::clone(&base_ctx.db));
    let now = ctx.now_unix();

    let (st_react_exact, st_react_mfg_fb, st_react_def_fb, st_destroyed, st_bad_sys) =
        (1_077_000_001_i64, 1_077_000_002, 1_077_000_003, 1_077_000_004, 1_077_000_005);
    let other_corp = 98_777_999_i64;

    ctx.db.with_mut_snapshot(|snap| {
        for (sid, sys, state) in [
            (st_react_exact, 30_000_142, "ShieldVulnerable"),
            (st_react_mfg_fb, 30_000_142, "ShieldVulnerable"),
            (st_react_def_fb, 30_000_142, "ShieldVulnerable"),
            (st_destroyed, 30_000_142, "Destroyed"),
            (st_bad_sys, 99_999_999, "ShieldVulnerable"),
        ] {
            snap.structures.insert(
                sid,
                UpwellStructureRecord {
                    structure_id: sid,
                    type_id: 35_835,
                    owner_corp_id: corp_id,
                    solar_system_id: sys,
                    name: format!("Struct {sid}"),
                    state: state.into(),
                    quantum_core_installed: true,
                    fuel_blocks: 100,
                    ..Default::default()
                },
            );
            let mod_id = sid + 500;
            snap.items.insert(
                mod_id,
                InventoryItemRecord {
                    item_id: mod_id,
                    type_id: 45_539,
                    owner_id: corp_id,
                    location_id: sid,
                    flag: InventoryFlag::ServiceSlot0,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
            snap.module_online_states.insert(mod_id, true);
        }
        save_facility_taxes(snap, st_react_exact, &HashMap::from([(9, 0.035)]));
        save_facility_taxes(snap, st_react_mfg_fb, &HashMap::from([(1, 0.025)]));
        save_facility_taxes(snap, st_react_def_fb, &HashMap::new());
        let mut ext = crate::services::station_structure::structure_upwell_ops::load_ext_state(snap);
        for sid in [st_react_exact, st_react_mfg_fb, st_react_def_fb] {
            ext.access_profiles.insert(
                sid,
                crate::services::station_structure::structure_upwell_ops::StructureAccessProfile {
                    corp_only: false,
                    ..Default::default()
                },
            );
        }
        crate::services::station_structure::structure_upwell_ops::save_ext_state(snap, &ext);

        snap.items.insert(
            555_001,
            InventoryItemRecord {
                item_id: 555_001,
                type_id: 688,
                owner_id: char_id,
                location_id: 60_003_760,
                flag: InventoryFlag::Locked,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );

        snap.industry_jobs.clear();
        snap.industry_jobs.insert(101, mk_job(101, char_id, non_ind_st_id, 5, 555_001, 688, 688, 4, 1_700_000_000, 600, "Active"));
        save_job_cost_meta(snap, 101, &IndustryJobCostMeta { total_cost_isk: 750.0, inv_prob: 0.35, paused_at_unix: Some(1_700_000_100), blueprint_location_id: 9_001, output_location_id: 9_002, ..Default::default() });

        snap.industry_jobs.insert(102, mk_job(102, char_id, 60_003_760, 8, 555_001, 688, 11_372, 2, 1_700_000_000, 500, "Delivered"));
        save_job_cost_meta(snap, 102, &IndustryJobCostMeta { invented_runs: 3, ..Default::default() });

        snap.industry_jobs.insert(103, mk_job(103, char_id, 60_003_760, 1, 999_999, 688, 587, 5, 1_700_000_000, 100, "Cancelled"));
        save_job_cost_meta(snap, 103, &IndustryJobCostMeta { use_corp: true, owner_id: corp_id, location_id: 8_888, completed_unix: Some(1_700_000_050), ..Default::default() });

        snap.industry_jobs.insert(104, mk_job(104, char_id, 60_003_760, 8, 555_001, 688, 11_372, 1, 1_700_000_000, 100, "Failed"));

        let mining_recent = now.saturating_sub(29 * 86_400 + 86_000);
        let mining_expired = now.saturating_sub(30 * 86_400 + 60);
        snap.mining_ledger = vec![
            MiningLedgerRecord { character_id: char_id, solar_system_id: 30_000_142, type_id: 1230, quantity: 100, date: String::new(), timestamp_unix: mining_recent },
            MiningLedgerRecord { character_id: char_id, solar_system_id: 30_000_142, type_id: 1230, quantity: 250, date: format_date_utc(mining_recent), timestamp_unix: now },
            MiningLedgerRecord { character_id: char_id, solar_system_id: 30_000_142, type_id: 1230, quantity: 999, date: String::new(), timestamp_unix: mining_expired },
            MiningLedgerRecord { character_id: char_id, solar_system_id: 30_000_142, type_id: 1230, quantity: 0, date: String::new(), timestamp_unix: now },
        ];

        snap.moon_extractions.clear();
        for (eid, status) in [(1_i64, "Started"), (2, "Cancelled")] {
            snap.moon_extractions.insert(
                eid,
                MoonExtractionRecord {
                    structure_id: st_react_exact,
                    moon_id: 40_000_001,
                    extraction_start_unix: 1_700_000_000,
                    chunk_arrival_unix: 1_700_100_000,
                    natural_decay_unix: 1_700_200_000,
                    status: status.into(),
                    auto_fracture: false,
                    event_id: eid,
                    moon_ores: vec![],
                    spawned_asteroid_ids: vec![],
                },
            );
        }

        snap.observer_mining_ledger.clear();
        snap.observer_mining_ledger.insert(st_react_mfg_fb, vec![]);
        snap.observer_mining_ledger.insert(
            st_react_exact,
            vec![
                ObserverMiningRecord { observer_id: st_react_exact, character_id: char_id + 1, recorded_corporation_id: other_corp, type_id: 45_490, quantity: 300, last_updated_date: String::new(), timestamp_unix: 1_700_000_000 },
                ObserverMiningRecord { observer_id: st_react_exact, character_id: char_id, recorded_corporation_id: other_corp, type_id: 45_491, quantity: 150, last_updated_date: "2024-01-15".into(), timestamp_unix: 1_705_276_800 },
            ],
        );
        snap.observer_mining_ledger.insert(
            st_destroyed,
            vec![ObserverMiningRecord { observer_id: st_destroyed, character_id: char_id, recorded_corporation_id: other_corp, type_id: 45_490, quantity: 50, last_updated_date: "2024-01-01".into(), timestamp_unix: 1_700_000_000 }],
        );
    });

    let empty = HashMap::new();
    let char_s = char_id.to_string();
    let corp_s = corp_id.to_string();
    let p_char = params(&[("character_id", &char_s)]);
    let p_corp = params(&[("corporation_id", &corp_s)]);

    let facs = dispatch_economy_industry_route(&ctx, "GET", "/industry/facilities", &empty, &empty, None, 1).unwrap();
    let fac_arr = body(&facs).as_array().unwrap();
    let tax_of = |fid: i64| fac_arr.iter().find(|f| f["facility_id"] == fid).map(|f| f["tax"].as_f64().unwrap());
    assert_eq!(tax_of(st_react_exact), Some(0.035));
    assert_eq!(tax_of(st_react_mfg_fb), Some(0.025));
    assert_eq!(tax_of(st_react_def_fb), Some(0.05));
    assert_eq!(tax_of(non_ind_st_id), Some(NPC_STATION_INDUSTRY_TAX_RATE));
    assert_eq!(tax_of(st_destroyed), None);
    assert_eq!(tax_of(st_bad_sys), None);

    // Private corp-only structure with industry service must be hidden from unauthenticated /industry/facilities
    let st_private = 1_077_000_006_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            st_private,
            UpwellStructureRecord {
                structure_id: st_private,
                type_id: 35_835,
                owner_corp_id: corp_id,
                solar_system_id: 30_000_142,
                name: "Private Industry Citadel".into(),
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 100,
                ..Default::default()
            },
        );
        let mod_id = st_private + 500;
        snap.items.insert(
            mod_id,
            InventoryItemRecord {
                item_id: mod_id,
                type_id: 45_539,
                owner_id: corp_id,
                location_id: st_private,
                flag: InventoryFlag::ServiceSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.module_online_states.insert(mod_id, true);
        let mut ext = crate::services::station_structure::structure_upwell_ops::load_ext_state(snap);
        ext.access_profiles.insert(
            st_private,
            crate::services::station_structure::structure_upwell_ops::StructureAccessProfile {
                corp_only: true,
                ..Default::default()
            },
        );
        crate::services::station_structure::structure_upwell_ops::save_ext_state(snap, &ext);
    });
    let facs_after = dispatch_economy_industry_route(&ctx, "GET", "/industry/facilities", &empty, &empty, None, 1).unwrap();
    assert!(!body(&facs_after).as_array().unwrap().iter().any(|f| f["facility_id"] == st_private));

    assert_eq!(dispatch_economy_industry_route(&ctx, "GET", "/characters/{character_id}/industry/jobs", &empty, &empty, None, 1).unwrap().status_code, 400);
    assert_eq!(dispatch_economy_industry_route(&ctx, "GET", "/characters/{character_id}/industry/jobs", &params(&[("character_id", "99999999")]), &empty, None, 1).unwrap().status_code, 404);
    let act_only = dispatch_economy_industry_route(&ctx, "GET", "/characters/{character_id}/industry/jobs", &p_char, &params(&[("include_completed", "false")]), None, 1).unwrap();
    let act_arr = body(&act_only).as_array().unwrap();
    assert_eq!(act_arr.len(), 1);
    assert_eq!(act_arr[0]["job_id"], 101);
    assert_eq!(act_arr[0]["licensed_runs"], 4);
    assert_eq!(act_arr[0]["cost"], 750.0);
    assert_eq!(act_arr[0]["probability"], 0.35);
    assert_eq!(act_arr[0]["blueprint_location_id"], 9_001);
    assert_eq!(act_arr[0]["output_location_id"], 9_002);
    assert!(act_arr[0]["pause_date"].is_string() && act_arr[0]["station_id"] == non_ind_st_id);

    let all_char_jobs = dispatch_economy_industry_route(&ctx, "GET", "/characters/{character_id}/industry/jobs", &p_char, &empty, None, 1).unwrap();
    let all_arr = body(&all_char_jobs).as_array().unwrap();
    assert_eq!(all_arr.len(), 3);
    assert_eq!(all_arr[1]["licensed_runs"], 3);
    assert_eq!(all_arr[1]["successful_runs"], 2);
    assert_eq!(all_arr[2]["successful_runs"], 0);
    assert!(all_arr[2].get("completed_date").is_none());

    assert_eq!(dispatch_economy_industry_route(&ctx, "GET", "/corporations/{corporation_id}/industry/jobs", &empty, &empty, None, 1).unwrap().status_code, 400);
    assert_eq!(dispatch_economy_industry_route(&ctx, "GET", "/corporations/{corporation_id}/industry/jobs", &params(&[("corporation_id", "99999999")]), &empty, None, 1).unwrap().status_code, 404);
    assert_eq!(dispatch_economy_industry_route(&ctx, "GET", "/corporations/{corporation_id}/industry/jobs", &p_corp, &empty, None, 0).unwrap().status_code, 400);
    let corp_jobs = dispatch_economy_industry_route(&ctx, "GET", "/corporations/{corporation_id}/industry/jobs", &p_corp, &empty, None, 1).unwrap();
    let cj_arr = body(&corp_jobs).as_array().unwrap();
    assert_eq!(cj_arr.len(), 1);
    assert_eq!(cj_arr[0]["job_id"], 103);
    assert_eq!(cj_arr[0]["location_id"], 8_888);
    assert!(cj_arr[0]["completed_date"].is_string() && cj_arr[0]["successful_runs"] == 0);

    assert_eq!(dispatch_economy_industry_route(&ctx, "GET", "/characters/{character_id}/mining", &empty, &empty, None, 1).unwrap().status_code, 400);
    assert_eq!(dispatch_economy_industry_route(&ctx, "GET", "/characters/{character_id}/mining", &params(&[("character_id", "99999999")]), &empty, None, 1).unwrap().status_code, 404);
    assert_eq!(dispatch_economy_industry_route(&ctx, "GET", "/characters/{character_id}/mining", &p_char, &empty, None, 0).unwrap().status_code, 400);
    let mining = dispatch_economy_industry_route(&ctx, "GET", "/characters/{character_id}/mining", &p_char, &empty, None, 1).unwrap();
    assert_eq!(body(&mining).as_array().unwrap()[0]["quantity"], 350);

    for route in ["/corporation/{corporation_id}/mining/extractions", "/corporation/{corporation_id}/mining/observers"] {
        assert_eq!(dispatch_economy_industry_route(&ctx, "GET", route, &empty, &empty, None, 1).unwrap().status_code, 400);
        assert_eq!(dispatch_economy_industry_route(&ctx, "GET", route, &params(&[("corporation_id", "99999999")]), &empty, None, 1).unwrap().status_code, 404);
        assert_eq!(dispatch_economy_industry_route(&ctx, "GET", route, &p_corp, &empty, None, 0).unwrap().status_code, 400);
    }
    let exts = dispatch_economy_industry_route(&ctx, "GET", "/corporation/{corporation_id}/mining/extractions", &p_corp, &empty, None, 1).unwrap();
    assert_eq!(body(&exts).as_array().unwrap().len(), 1);

    let obs_list = dispatch_economy_industry_route(&ctx, "GET", "/corporation/{corporation_id}/mining/observers", &p_corp, &empty, None, 1).unwrap();
    let obs_arr = body(&obs_list).as_array().unwrap();
    assert_eq!(obs_arr.len(), 1);
    assert_eq!(obs_arr[0]["last_updated"], "2024-01-15");

    let obs_ep = "/corporation/{corporation_id}/mining/observers/{observer_id}";
    assert_eq!(dispatch_economy_industry_route(&ctx, "GET", obs_ep, &empty, &empty, None, 1).unwrap().status_code, 400);
    assert_eq!(dispatch_economy_industry_route(&ctx, "GET", obs_ep, &p_corp, &empty, None, 1).unwrap().status_code, 400);
    assert_eq!(dispatch_economy_industry_route(&ctx, "GET", obs_ep, &params(&[("corporation_id", "99999999"), ("observer_id", "1")]), &empty, None, 1).unwrap().status_code, 404);
    assert_eq!(dispatch_economy_industry_route(&ctx, "GET", obs_ep, &params(&[("corporation_id", &corp_s), ("observer_id", "999999")]), &empty, None, 1).unwrap().status_code, 404);
    assert_eq!(dispatch_economy_industry_route(&ctx, "GET", obs_ep, &params(&[("corporation_id", &corp_s), ("observer_id", &st_destroyed.to_string())]), &empty, None, 1).unwrap().status_code, 404);
    assert_eq!(dispatch_economy_industry_route(&ctx, "GET", obs_ep, &params(&[("corporation_id", &corp_s), ("observer_id", &st_react_exact.to_string())]), &empty, None, 0).unwrap().status_code, 400);
    let obs_det = dispatch_economy_industry_route(&ctx, "GET", obs_ep, &params(&[("corporation_id", &corp_s), ("observer_id", &st_react_exact.to_string())]), &empty, None, 1).unwrap();
    assert_eq!(body(&obs_det).as_array().unwrap()[0]["last_updated"], "2023-11-14");
    assert!(dispatch_economy_industry_route(&ctx, "GET", "/unknown/route", &empty, &empty, None, 1).is_none());
}
