use super::*;
use crate::router::ServerContext;
use eve_db::{
    IndustryJobRecord, InventoryFlag, InventoryItemRecord, PlanetaryColonyRecord,
    TrainedSkillRecord, WorldRepository,
};
use eve_proto::PyRep;
use eve_sde::{
    InvType, PlanetSchematicInfo, PlanetSchematicTypeEntry, SdeRegistry, StargateJump,
};
use glam::DVec3;
use std::collections::HashMap;
use std::sync::Arc;

fn make_test_context() -> ServerContext {
    let sde = Arc::new(SdeRegistry::with_canonical_seed());
    let db = Arc::new(WorldRepository::with_demo_seed());
    ServerContext::with_sde_and_repository(sde, db)
}

fn char_session(char_id: i64, corp_id: i64) -> SessionState {
    SessionState {
        user_id: Some(1),
        character_id: Some(char_id),
        corporation_id: Some(corp_id),
        solar_system_id: Some(30_000_142),
        station_id: Some(60_003_760),
        ..Default::default()
    }
}

fn make_inv_type(
    type_id: u32,
    group_id: u32,
    name: &str,
    default_attributes: HashMap<u16, f64>,
) -> InvType {
    InvType {
        type_id,
        group_id,
        category_id: 43,
        name: name.into(),
        mass: 1.0,
        volume: 1.0,
        capacity: 0.0,
        radius: 10.0,
        base_price: 10.0,
        published: true,
        portion_size: 1,
        market_group_id: None,
        race_id: None,
        faction_id: None,
        variation_parent_type_id: None,
        description_id: None,
        icon_id: None,
        graphic_id: None,
        sound_id: None,
        meta_group_id: None,
        meta_level: None,
        tech_level: None,
        default_effect_id: None,
        description: String::new(),
        effect_ids: Vec::new(),
        default_attributes,
    }
}

fn setup_pi_planet_sde(ctx: &mut ServerContext) -> (u32, i64) {
    let sys_id = 30_000_142;
    let planet_id = *ctx.sde.solar_systems[&sys_id]
        .planet_ids
        .first()
        .expect("Jita has planets");
    let planet_type_id = ctx
        .sde
        .celestials_in_system(sys_id)
        .into_iter()
        .find(|c| c.celestial_id == planet_id)
        .expect("planet celestial")
        .type_id;
    let mut sde = (*ctx.sde).clone();
    sde.types.insert(
        2268,
        make_inv_type(
            2268,
            1032,
            "Aqueous Liquids",
            HashMap::from([(ATTR_EXPORT_TAX_MULTIPLIER, 5.0)]),
        ),
    );
    sde.types.insert(
        3060,
        make_inv_type(
            3060,
            1026,
            "Custom Extractor Control Unit",
            HashMap::from([
                (ATTR_PLANET_RESTRICTION, f64::from(planet_type_id)),
                (ATTR_HARVESTER_TYPE, 2268.0),
                (ATTR_PIN_EXTRACTION_QUANTITY, 1000.0),
                (ATTR_PIN_CYCLE_TIME, 300.0),
            ]),
        ),
    );
    ctx.sde = Arc::new(sde);
    (sys_id, planet_id)
}

#[test]
fn jump_distance_decryptors_overlap_schematics_and_tax_bonus_work() {
    let mut ctx = make_test_context();
    let mut sde = (*ctx.sde).clone();
    sde.jumps.insert(
        1,
        StargateJump {
            stargate_id: 1,
            from_solar_system_id: 30_000_142,
            to_solar_system_id: 30_000_144,
            destination_stargate_id: 2,
            position: DVec3::ZERO,
        },
    );
    sde.jumps.insert(
        3,
        StargateJump {
            stargate_id: 3,
            from_solar_system_id: 30_000_144,
            to_solar_system_id: 30_002_187,
            destination_stargate_id: 4,
            position: DVec3::ZERO,
        },
    );
    sde.types.insert(
        34201,
        make_inv_type(
            34201,
            1304,
            "Accelerant Decryptor",
            HashMap::from([(1112, 1.2), (1113, 2.0), (1114, 10.0), (1124, 1.0)]),
        ),
    );
    sde.planet_schematics.insert(
        121,
        PlanetSchematicInfo {
            schematic_id: 121,
            name: "Water".into(),
            cycle_time: 1800,
            types: vec![
                PlanetSchematicTypeEntry {
                    type_id: 2268,
                    quantity: 3000,
                    is_input: true,
                },
                PlanetSchematicTypeEntry {
                    type_id: 2395,
                    quantity: 20,
                    is_input: false,
                },
            ],
            pins: vec![2470],
        },
    );
    ctx.sde = Arc::new(sde);

    assert_eq!(
        solar_system_jump_distance(&ctx, 30_000_142, 30_000_142),
        Some(0)
    );
    assert_eq!(
        solar_system_jump_distance(&ctx, 30_000_142, 30_000_144),
        Some(1)
    );
    assert!(solar_system_jump_distance(&ctx, 30_000_142, 30_002_187).is_some());
    assert_eq!(
        solar_system_jump_distance(&ctx, 30_000_142, 31_000_005),
        None
    );

    assert_eq!(
        resolve_decryptor_modifiers(&ctx, 999999).unwrap_err().code,
        "InvalidDecryptor"
    );
    let (prob, me, te, runs) = resolve_decryptor_modifiers(&ctx, 34201).unwrap();
    assert!((prob - 1.2).abs() < 1e-9);
    assert!((me - 2.0).abs() < 1e-9);
    assert!((te - 10.0).abs() < 1e-9);
    assert_eq!(runs, 1);

    assert!((extractor_overlap_factor(&[(1, 0.1, 0.1)], 0.05) - 1.0).abs() < 1e-9);
    assert!(
        (extractor_overlap_factor(&[(1, 0.0, 0.0), (2, 1.0, 1.0)], 0.05) - 1.0).abs() < 1e-9
    );
    let over = extractor_overlap_factor(&[(1, 0.1, 0.1), (2, 0.1, 0.1)], 0.05);
    assert!((over - 0.65).abs() < 1e-9);

    let (ct, ins, out_t, out_q) = pi_schematic_recipe(&ctx.sde, 121).unwrap();
    assert_eq!(
        (ct, ins, out_t, out_q),
        (1800, vec![(2268, 3000)], 2395, 20)
    );
    assert!(pi_schematic_recipe(&ctx.sde, 999999).is_none());

    assert!((compute_facility_tax_bonus_for_tier(0.10, 0) - 0.10).abs() < 1e-9);
    assert!((compute_facility_tax_bonus_for_tier(0.10, 3) - 0.07).abs() < 1e-9);
    assert!((compute_facility_tax_bonus_for_tier(0.10, 8) - 0.05).abs() < 1e-9);
}

#[test]
fn planet_mgr_resource_rpcs_and_p0_extraction_resolve_correctly() {
    let mut ctx = make_test_context();
    let (sys_id, planet_id) = setup_pi_planet_sde(&mut ctx);
    let char_id = 90_000_001;
    let mut session = char_session(char_id, 98_000_001);

    let (p0_tid, cycle, qty, tax) =
        resolve_sde_planet_p0_extraction(&ctx, sys_id, planet_id, None).unwrap();
    assert!(p0_tid > 0 && cycle > 0 && qty > 0 && tax >= 0.0);
    assert_eq!(
        resolve_sde_planet_p0_extraction(&ctx, sys_id, planet_id, Some(999999))
            .unwrap_err()
            .code,
        "ClientSchematicOverrideForbidden"
    );
    assert!(list_sde_planet_p0_resources(&ctx, sys_id, planet_id).contains(&2268));

    let info_rep = handle(
        &ctx,
        &mut session,
        "planetMgr",
        "GetPlanetInfo",
        &[PyRep::Integer(planet_id)],
        &HashMap::new(),
        None,
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        info_rep.dict_get("planetID").unwrap().as_i64(),
        Some(planet_id)
    );
    assert_eq!(
        info_rep.dict_get("hasColony").unwrap().as_bool(),
        Some(false)
    );
    let expected_cel = ctx
        .sde
        .celestials_in_system(sys_id)
        .into_iter()
        .find(|c| c.celestial_id == planet_id && c.group_id == 7)
        .expect("planet celestial in SDE");
    assert_eq!(
        info_rep.dict_get("celestialIndex").and_then(|v| v.as_i64()),
        Some(i64::from(expected_cel.celestial_index.unwrap_or(1)))
    );
    if expected_cel.radius > 0.0 {
        assert_eq!(
            info_rep.dict_get("radius").and_then(|v| v.as_f64()),
            Some(expected_cel.radius)
        );
    }
    let Some(PyRep::List(info_resources)) = info_rep.dict_get("resources") else {
        panic!("GetPlanetInfo must include resources list");
    };
    assert!(!info_resources.is_empty());
    for entry in info_resources {
        let tid = entry.dict_get("resourceTypeID").unwrap().as_i64().unwrap() as TypeId;
        let q = entry.dict_get("quality").unwrap().as_f64().unwrap();
        let expected_q = ctx
            .db
            .with_snapshot(|snap| compute_planet_resource_quality(&ctx, snap, planet_id, tid));
        assert!((q - expected_q).abs() < 1e-12);
    }

    let res_info = handle(
        &ctx,
        &mut session,
        "planetMgr",
        "GetPlanetResourceInfo",
        &[PyRep::Integer(planet_id)],
        &HashMap::new(),
        None,
    )
    .unwrap()
    .unwrap();
    let PyRep::Dict(res_entries) = res_info else {
        panic!("GetPlanetResourceInfo must return PyRep::Dict, got {res_info:?}");
    };
    assert!(res_entries.len() >= 2);
    for (k, v) in &res_entries {
        assert!(k.as_i64().unwrap_or(0) > 0);
        let q = v.as_f64().unwrap_or(0.0);
        assert!(
            q > 0.0 && q <= MAX_DISPLAY_QUALITY,
            "unexpected V24 wire quality {q} (expected (0, {MAX_DISPLAY_QUALITY}])"
        );
        let norm = q / MAX_DISPLAY_QUALITY;
        assert!(norm > 0.0 && norm <= 1.0, "normalized quality {norm} out of bounds");
    }

    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.remove(&char_id);
    });

    let err_no_rs = handle(
        &ctx,
        &mut session,
        "planetMgr",
        "GetResourceData",
        &[PyRep::Integer(planet_id), PyRep::Integer(2268)],
        &HashMap::new(),
        None,
    )
    .unwrap()
    .unwrap_err();
    assert_eq!(err_no_rs.code, "PISkillRequiredToSurveyPlanetResources");

    ctx.db.with_mut_snapshot(|snap| {
        for (sid, lvl) in [(13279, 3), (2406, 4), (2403, 2)] {
            snap.skills.entry(char_id).or_default().insert(
                sid,
                TrainedSkillRecord {
                    type_id: sid,
                    level: lvl,
                    skill_points: 100_000.0,
                },
            );
        }
        snap.planetary_colonies.insert(
            (char_id, planet_id),
            PlanetaryColonyRecord {
                owner_id: char_id,
                planet_id,
                solar_system_id: sys_id,
                command_center_level: 2,
                pins_count: 3,
                last_extracted_unix: 1000,
                extracted_commodities: HashMap::new(),
            },
        );
    });

    let res_data = handle(
        &ctx,
        &mut session,
        "planetMgr",
        "GetResourceData",
        &[PyRep::keyval([
            ("planetID", PyRep::Integer(planet_id)),
            ("resourceTypeID", PyRep::Integer(2268)),
            ("newBand", PyRep::Integer(6)),
            ("proximity", PyRep::Integer(3)),
        ])],
        &HashMap::new(),
        None,
    )
    .unwrap()
    .unwrap();
    assert_eq!(res_data.dict_get("numBands").unwrap().as_i64(), Some(6));
    assert_eq!(res_data.dict_get("proximity").unwrap().as_i64(), Some(4));
    let Some(PyRep::Bytes(sh_bytes)) = res_data.dict_get("data") else {
        panic!("GetResourceData must include binary SH float buffer");
    };
    assert_eq!(sh_bytes.len(), 6 * 6 * 4);
    let sh_coeffs: Vec<f32> = sh_bytes
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect();
    assert!(sh_coeffs.iter().all(|v| v.is_finite()));
    assert!(sh_coeffs[0] > 0.0, "l=0 monopole coefficient must be positive");
    assert!(
        sh_coeffs[1..4].iter().any(|c| c.abs() > 1e-5),
        "l=1 spherical harmonic dipole band must be non-degenerate"
    );

    let planets_rep = handle(
        &ctx,
        &mut session,
        "planetMgr",
        "GetPlanetsForChar",
        &[],
        &HashMap::new(),
        None,
    )
    .unwrap()
    .unwrap();
    let PyRep::Rowset { columns, rows } = &planets_rep else {
        panic!("GetPlanetsForChar must return Rowset, got {planets_rep:?}");
    };
    assert_eq!(
        columns,
        &vec![
            "planetID",
            "solarSystemID",
            "planetTypeID",
            "celestialIndex",
            "numberOfPins",
            "commandCenterLevel",
            "colonyData",
        ]
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][0].as_i64(), Some(planet_id));
    assert_eq!(rows[0][4].as_i64(), Some(1));
    let Some(PyRep::List(pins)) = rows[0][6].dict_get("pins") else {
        panic!("expected colonyData.pins list, got {:?}", rows[0][6]);
    };
    assert_eq!(pins.len(), 1);
}

#[test]
fn industry_job_queries_and_monitor_connect_disconnect_work() {
    let ctx = make_test_context();
    let char_id = 90_000_001;
    let station_id = 60_003_760;
    let mut session = char_session(char_id, 98_000_001);

    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.remove(&char_id);
        snap.industry_jobs.clear();
        snap.industry_jobs.insert(
            501,
            IndustryJobRecord {
                job_id: 501,
                installer_id: char_id,
                facility_id: station_id,
                blueprint_item_id: 8001,
                blueprint_type_id: 688,
                activity_id: 1,
                runs: 2,
                product_type_id: 587,
                status: "Active".into(),
                duration_seconds: 600,
                start_unix: 1000,
                blueprint_location_id: station_id,
                output_location_id: station_id,
                cost: Some(150.0),
                licensed_runs: None,
                probability: None,
                completed_unix: None,
                completed_character_id: None,
            },
        );
        snap.items.insert(
            8002,
            InventoryItemRecord {
                item_id: 8002,
                type_id: 34,
                owner_id: char_id,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 1200,
                singleton: false,
                ..Default::default()
            },
        );
        snap.items.insert(
            8003,
            InventoryItemRecord {
                item_id: 8003,
                type_id: 36,
                owner_id: char_id,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 444,
                singleton: false,
                ..Default::default()
            },
        );
    });

    let counts = handle_industry_job_queries(
        &ctx,
        &mut session,
        "industryManager",
        "GetJobCounts",
        &[],
        &HashMap::new(),
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        counts.dict_get("manufacturingUsed").unwrap().as_i64(),
        Some(1)
    );
    assert_eq!(
        counts.dict_get("manufacturingMax").unwrap().as_i64(),
        Some(1)
    );

    let job_rep = handle_industry_job_queries(
        &ctx,
        &mut session,
        "industryManager",
        "GetJob",
        &[PyRep::Integer(501)],
        &HashMap::new(),
    )
    .unwrap()
    .unwrap();
    assert_eq!(job_rep.dict_get("jobID").unwrap().as_i64(), Some(501));

    let jobs_by_owner = handle_industry_job_queries(
        &ctx,
        &mut session,
        "industryManager",
        "GetJobsByOwner",
        &[PyRep::Integer(char_id)],
        &HashMap::new(),
    )
    .unwrap()
    .unwrap();
    assert!(matches!(jobs_by_owner, PyRep::List(v) if v.len() == 1));

    let conn = handle_industry_job_queries(
        &ctx,
        &mut session,
        "industryMonitor",
        "ConnectJob",
        &[PyRep::Integer(501)],
        &HashMap::new(),
    )
    .unwrap()
    .unwrap();
    let (monitor_id, station_avail) = match conn {
        PyRep::Tuple(v) => (v[0].as_i64().unwrap(), v[1].clone()),
        _ => panic!("expected tuple"),
    };
    let PyRep::Dict(ref station_entries) = station_avail else {
        panic!("expected Dict from ConnectJob(501)");
    };
    assert!(
        station_entries.iter().any(|(k, v)| k.as_i64() == Some(34) && v.as_i64().is_some_and(|q| q >= 1200)),
        "station monitor must report >= 1200 Tritanium: {station_entries:?}"
    );
    assert!(
        station_entries.iter().any(|(k, v)| k.as_i64() == Some(36) && v.as_i64().is_some_and(|q| q >= 444)),
        "station monitor must report >= 444 Mexallon: {station_entries:?}"
    );
    let disc = handle_industry_job_queries(
        &ctx,
        &mut session,
        "industryMonitor",
        "DisconnectJob",
        &[PyRep::Integer(monitor_id)],
        &HashMap::new(),
    )
    .unwrap()
    .unwrap();
    assert_eq!(disc.as_bool(), Some(true));

    // DisconnectJob(None), empty args, or <= 0 must succeed as Ok(None) instead of raising JobNotFound (#661)
    for args in [vec![PyRep::None], vec![], vec![PyRep::Integer(0)], vec![PyRep::Integer(-1)]] {
        let res = handle_industry_job_queries(
            &ctx,
            &mut session,
            "industryMonitor",
            "DisconnectJob",
            &args,
            &HashMap::new(),
        )
        .unwrap()
        .expect("DisconnectJob(None/empty/non-positive) must return Ok(None)");
        assert_eq!(res, PyRep::None);
    }

    // Nested inputLocation ObjectEx (client job.dump() format) resolves container itemID, flagID, and ownerID
    let container_id = 990_555_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            990_556,
            InventoryItemRecord {
                item_id: 990_556,
                type_id: 34,
                owner_id: 90_000_001,
                location_id: container_id,
                flag: InventoryFlag::Hangar,
                quantity: 3000,
                singleton: false,
                ..Default::default()
            },
        );
    });
    let nested_job_dump = PyRep::Dict(vec![
        (PyRep::String("facilityID".into()), PyRep::Integer(60_003_760)),
        (
            PyRep::String("inputLocation".into()),
            PyRep::ObjectEx {
                is_type_2: true,
                header: Box::new(PyRep::Tuple(vec![
                    PyRep::Tuple(vec![PyRep::Token("industry.job.Location".into())]),
                    PyRep::Dict(vec![
                        (PyRep::String("itemID".into()), PyRep::Integer(container_id)),
                        (
                            PyRep::String("flagID".into()),
                            PyRep::Integer(InventoryFlag::Hangar as i64),
                        ),
                        (PyRep::String("ownerID".into()), PyRep::Integer(90_000_001)),
                    ]),
                ])),
                list: Vec::new(),
                dict: Vec::new(),
            },
        ),
    ]);
    let nested_conn = handle_industry_job_queries(
        &ctx,
        &mut session,
        "industryMonitor",
        "ConnectJob",
        &[nested_job_dump],
        &HashMap::new(),
    )
    .unwrap()
    .unwrap();
    let PyRep::Tuple(nested_parts) = nested_conn else {
        panic!("expected tuple from nested inputLocation ConnectJob");
    };
    let nested_mon_id = nested_parts[0].as_i64().unwrap();
    assert_eq!(
        nested_parts[1],
        PyRep::Dict(vec![(PyRep::Integer(34), PyRep::Integer(3000))])
    );
    let _ = handle_industry_job_queries(
        &ctx,
        &mut session,
        "industryMonitor",
        "DisconnectJob",
        &[PyRep::Integer(nested_mon_id)],
        &HashMap::new(),
    );

    // Zero/placeholder IDs inside nested inputLocation fall back cleanly to facilityID / Hangar / char_id
    let zero_id_job_dump = PyRep::Dict(vec![
        (PyRep::String("facilityID".into()), PyRep::Integer(station_id)),
        (
            PyRep::String("inputLocation".into()),
            PyRep::ObjectEx {
                is_type_2: true,
                header: Box::new(PyRep::Tuple(vec![
                    PyRep::Tuple(vec![PyRep::Token("industry.job.Location".into())]),
                    PyRep::Dict(vec![
                        (PyRep::String("itemID".into()), PyRep::Integer(0)),
                        (PyRep::String("flagID".into()), PyRep::Integer(0)),
                        (PyRep::String("ownerID".into()), PyRep::Integer(0)),
                    ]),
                ])),
                list: Vec::new(),
                dict: Vec::new(),
            },
        ),
    ]);
    let zero_conn = handle_industry_job_queries(
        &ctx,
        &mut session,
        "industryMonitor",
        "ConnectJob",
        &[zero_id_job_dump],
        &HashMap::new(),
    )
    .unwrap()
    .unwrap();
    let PyRep::Tuple(zero_parts) = zero_conn else {
        panic!("expected tuple from zero-ID fallback ConnectJob");
    };
    let zero_mon_id = zero_parts[0].as_i64().unwrap();
    assert_eq!(
        zero_parts[1], station_avail,
        "itemID/flagID/ownerID == 0 in inputLocation must fall back to facilityID/Hangar/char_id"
    );
    let zero_disc = handle_industry_job_queries(
        &ctx,
        &mut session,
        "industryMonitor",
        "DisconnectJob",
        &[PyRep::Integer(zero_mon_id)],
        &HashMap::new(),
    )
    .unwrap()
    .unwrap();
    assert_eq!(zero_disc.as_bool(), Some(true));
}

#[test]
fn industry_job_queries_resolve_real_solar_system_and_fail_closed_when_unresolvable() {
    let ctx = make_test_context();
    let char_id = 90_000_001_i64;
    let mut session = char_session(char_id, 98_000_001);
    let (&non_jita_station_id, non_jita_station) = ctx
        .sde
        .stations
        .iter()
        .find(|(_, st)| st.solar_system_id != 30_000_142)
        .expect("canonical SDE contains non-Jita station");
    let station_sys = non_jita_station.solar_system_id;
    let perimeter_sys = 30_000_144_u32;
    let struct_id = 1_099_119_701_i64;
    let pos_item_id = 1_099_119_702_i64;
    let missing_facility_id = 1_099_119_799_i64;

    let mk_job = |job_id: i64, facility_id: i64| IndustryJobRecord {
        job_id,
        installer_id: char_id,
        facility_id,
        blueprint_item_id: 90_000 + job_id,
        blueprint_type_id: 688,
        activity_id: 1,
        runs: 1,
        product_type_id: 587,
        status: "Active".into(),
        duration_seconds: 600,
        start_unix: 1_000,
        blueprint_location_id: facility_id,
        output_location_id: facility_id,
        cost: Some(100.0),
        licensed_runs: None,
        probability: None,
        completed_unix: None,
        completed_character_id: None,
    };

    // Unbound helper without DB/SDE context must emit PyRep::None rather than hardcoded Jita.
    let uncontextualized = format_industry_job_pyrep(
        &mk_job(9100, missing_facility_id),
        &IndustryJobCostMeta::default(),
    );
    assert_eq!(uncontextualized.dict_get("solarSystemID"), Some(&PyRep::None));

    ctx.db.with_mut_snapshot(|snap| {
        snap.industry_jobs.clear();
        snap.structures.insert(
            struct_id,
            eve_db::UpwellStructureRecord {
                structure_id: struct_id,
                type_id: 35_825,
                owner_corp_id: 98_000_001,
                solar_system_id: perimeter_sys,
                name: "Perimeter Raitaru".into(),
                state: "ShieldVulnerable".into(),
                ..Default::default()
            },
        );
        snap.items.insert(
            pos_item_id,
            InventoryItemRecord {
                item_id: pos_item_id,
                type_id: 24570,
                owner_id: 98_000_001,
                location_id: i64::from(perimeter_sys),
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.industry_jobs
            .insert(9101, mk_job(9101, non_jita_station_id));
        snap.industry_jobs.insert(9102, mk_job(9102, struct_id));
        snap.industry_jobs.insert(9103, mk_job(9103, pos_item_id));
        snap.industry_jobs
            .insert(9104, mk_job(9104, missing_facility_id));
    });

    for (job_id, expected_sys) in [
        (9101, station_sys),
        (9102, perimeter_sys),
        (9103, perimeter_sys),
    ] {
        let rep = handle_industry_job_queries(
            &ctx,
            &mut session,
            "industryManager",
            "GetJob",
            &[PyRep::Integer(job_id)],
            &HashMap::new(),
        )
        .unwrap()
        .expect("resolvable job succeeds");
        assert_eq!(
            rep.dict_get("solarSystemID"),
            Some(&PyRep::Integer(i64::from(expected_sys))),
            "job {job_id} must report its real solarSystemID"
        );
    }

    let err = handle_industry_job_queries(
        &ctx,
        &mut session,
        "industryManager",
        "GetJob",
        &[PyRep::Integer(9104)],
        &HashMap::new(),
    )
    .unwrap()
    .expect_err("unresolvable facility must fail closed instead of falling back to Jita");
    assert_eq!(err.code, "FacilityNotFound");

    let list_rep = handle_industry_job_queries(
        &ctx,
        &mut session,
        "industryManager",
        "GetJobsByOwner",
        &[PyRep::Integer(char_id)],
        &HashMap::new(),
    )
    .unwrap()
    .unwrap();
    let PyRep::List(jobs) = list_rep else {
        panic!("expected List from GetJobsByOwner");
    };
    assert_eq!(
        jobs.len(),
        3,
        "unresolvable-facility job 9104 must be omitted instead of synthesizing Jita"
    );
    let mut by_id = HashMap::new();
    for job in jobs {
        let jid = job.dict_get("jobID").and_then(PyRep::as_i64).unwrap();
        let sys = job.dict_get("solarSystemID").and_then(PyRep::as_i64).unwrap();
        by_id.insert(jid, sys);
    }
    assert_eq!(by_id.get(&9101), Some(&i64::from(station_sys)));
    assert_eq!(by_id.get(&9102), Some(&i64::from(perimeter_sys)));
    assert_eq!(by_id.get(&9103), Some(&i64::from(perimeter_sys)));
    assert!(!by_id.contains_key(&9104));
}

#[cfg(test)]
#[path = "industry_pi_extra_unit_tests.rs"]
mod industry_pi_extra_unit_tests;
