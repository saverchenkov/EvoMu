//! Extra branch-coverage and mutation-killing unit tests for `industry_pi`.

use super::industry_pi::*;
use crate::router::{BoundObjectEntry, ServerContext};
use eve_db::{
    CorporationRecord, IndustryJobRecord, InventoryFlag, InventoryItemRecord,
    PlanetaryColonyRecord, TrainedSkillRecord, WorldRepository, corp_roles,
};
use eve_machonet::{SessionState, service_roles};
use eve_proto::{BridgeRpcError, PyRep};
use eve_sde::{InvType, PlanetSchematicInfo, PlanetSchematicTypeEntry, SdeRegistry};
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

fn make_ctx() -> ServerContext {
    let db = Arc::new(WorldRepository::with_demo_seed());
    ServerContext::with_sde_and_repository(shared_sde(), db)
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

fn make_inv_type(type_id: u32, group_id: u32, attrs: HashMap<u16, f64>) -> InvType {
    InvType {
        type_id,
        group_id,
        category_id: 43,
        name: format!("Type-{type_id}"),
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
        default_attributes: attrs,
    }
}

fn call_pi_err(
    ctx: &ServerContext,
    session: &mut SessionState,
    svc: &str,
    method: &str,
    args: &[PyRep],
    bound: Option<&BoundObjectEntry>,
) -> BridgeRpcError {
    handle(ctx, session, svc, method, args, &HashMap::new(), bound)
        .expect("handled")
        .expect_err("expected error")
}

fn call_job_err(
    ctx: &ServerContext,
    session: &mut SessionState,
    svc: &str,
    method: &str,
    args: &[PyRep],
) -> BridgeRpcError {
    handle_industry_job_queries(ctx, session, svc, method, args, &HashMap::new())
        .expect("handled")
        .expect_err("expected error")
}

#[test]
fn decryptor_overlap_schematics_restore_flag_and_finalize_job_branches() {
    let mut ctx = make_ctx();
    let mut sde = (*ctx.sde).clone();

    sde.types.insert(950_001, make_inv_type(950_001, 999, HashMap::new()));
    sde.types.insert(950_002, make_inv_type(950_002, 1304, HashMap::from([(1112, -0.5), (1124, 2.6)])));

    for (sid, name, cyc, types) in [
        (
            901,
            "NoInputs",
            1800,
            vec![PlanetSchematicTypeEntry { type_id: 2395, quantity: 20, is_input: false }],
        ),
        (
            902,
            "NoOutputs",
            1800,
            vec![PlanetSchematicTypeEntry { type_id: 2268, quantity: 3000, is_input: true }],
        ),
        (
            903,
            "ZeroCycle",
            0,
            vec![
                PlanetSchematicTypeEntry { type_id: 2268, quantity: 3000, is_input: true },
                PlanetSchematicTypeEntry { type_id: 2395, quantity: 20, is_input: false },
            ],
        ),
        (
            904,
            "MultiInput",
            3600,
            vec![
                PlanetSchematicTypeEntry { type_id: 2397, quantity: 40, is_input: true },
                PlanetSchematicTypeEntry { type_id: 2395, quantity: 40, is_input: true },
                PlanetSchematicTypeEntry { type_id: 9836, quantity: 5, is_input: false },
            ],
        ),
    ] {
        sde.planet_schematics.insert(
            sid,
            PlanetSchematicInfo {
                schematic_id: sid,
                name: name.into(),
                cycle_time: cyc,
                pins: vec![],
                types,
            },
        );
    }
    ctx.sde = Arc::new(sde);

    assert_eq!(resolve_decryptor_modifiers(&ctx, 950_001).unwrap_err().code, "InvalidDecryptor");
    assert_eq!(resolve_decryptor_modifiers(&ctx, 950_002).unwrap(), (1.0, 0.0, 0.0, 3));

    assert_eq!(pi_schematic_recipe(&ctx.sde, 901), None);
    assert_eq!(pi_schematic_recipe(&ctx.sde, 902), None);
    assert_eq!(pi_schematic_recipe(&ctx.sde, 903), None);
    assert_eq!(pi_schematic_recipe(&ctx.sde, 904), Some((3600, vec![(2395, 40), (2397, 40)], 9836, 5)));

    assert_eq!(extractor_overlap_factor(&[], 0.05), 1.0);
    let half_overlap = extractor_overlap_factor(&[(1, 0.0, 0.0), (2, 0.05, 0.0)], 0.05);
    assert!((half_overlap - 0.825).abs() < 1e-9);
    assert_eq!(extractor_overlap_factor(&[(1, 0.0, 0.0), (2, 0.0, 0.0), (3, 0.0, 0.0), (4, 0.0, 0.0)], 0.05), 0.25);

    let mut meta = IndustryJobCostMeta::default();
    meta.blueprint_original_flag = Some(InventoryFlag::CorpSAG4);
    assert_eq!(restore_blueprint_flag(100, &meta), InventoryFlag::CorpSAG4);

    meta.blueprint_original_flag = Some(InventoryFlag::Locked);
    meta.use_corp = true;
    meta.owner_id = 98_000_001;
    assert_eq!(restore_blueprint_flag(98_000_001, &meta), InventoryFlag::CorpSAG1);

    meta.use_corp = false;
    meta.blueprint_original_flag = Some(InventoryFlag::Cargo);
    assert_eq!(restore_blueprint_flag(90_000_001, &meta), InventoryFlag::Cargo);

    meta.blueprint_original_flag = Some(InventoryFlag::None);
    assert_eq!(restore_blueprint_flag(90_000_001, &meta), InventoryFlag::Hangar);

    meta.owner_id = 98_000_001;
    meta.paused_at_unix = Some(1_500);
    let mut job = IndustryJobRecord {
        job_id: 701,
        installer_id: 90_000_001,
        facility_id: 60_003_760,
        blueprint_item_id: 9001,
        blueprint_type_id: 688,
        activity_id: 1,
        runs: 3,
        product_type_id: 587,
        status: "Paused".into(),
        duration_seconds: 600,
        start_unix: 1_000,
        blueprint_location_id: 60_003_760,
        output_location_id: 60_003_760,
        cost: Some(100.0),
        licensed_runs: None,
        probability: None,
        completed_unix: None,
        completed_character_id: None,
    };
    let rep = format_industry_job_pyrep(&job, &meta);
    assert_eq!(rep.dict_get("ownerID").unwrap().as_i64(), Some(98_000_001));
    assert_eq!(rep.dict_get("pauseUnix").unwrap().as_i64(), Some(1_500));
    assert_eq!(rep.dict_get("endUnix").unwrap().as_i64(), Some(1_600));

    let mfg_fin = finalize_completed_industry_job_response(&ctx, 90_000_001, "CompleteJob", 701, 9999, 587, &job).unwrap();
    assert_eq!(mfg_fin.dict_get("deliveredItemID").unwrap().as_i64(), Some(9999));
    assert_eq!(mfg_fin.dict_get("inventionSucceeded").unwrap().as_bool(), Some(false));

    job.activity_id = 8;
    assert_eq!(
        finalize_completed_industry_job_response(&ctx, 90_000_001, "CompleteJob", 701, 0, 587, &job).unwrap_err().code,
        "InventionAttemptFailed"
    );
    let inv_ok = finalize_completed_industry_job_response(&ctx, 90_000_001, "CompleteJob", 701, 8888, 587, &job).unwrap();
    assert_eq!(inv_ok.dict_get("inventionSucceeded").unwrap().as_bool(), Some(true));
}

#[test]
fn resolve_sde_planet_p0_extraction_and_planet_mgr_edge_cases() {
    let mut ctx = make_ctx();
    let sys_id = 30_000_142;
    let planet_id = *ctx.sde.solar_systems[&sys_id].planet_ids.first().expect("Jita has planets");
    let planet_type_id = ctx
        .sde
        .celestials_in_system(sys_id)
        .into_iter()
        .find(|c| c.celestial_id == planet_id && c.group_id == 7)
        .unwrap()
        .type_id;

    assert_eq!(resolve_sde_planet_p0_extraction(&ctx, sys_id, -999, None).unwrap_err().code, "PlanetNotInCurrentSystem");
    assert!(list_sde_planet_p0_resources(&ctx, sys_id, -999).is_empty());

    let mut sde = (*ctx.sde).clone();
    sde.types.retain(|_, t| t.group_id != 1026);
    ctx.sde = Arc::new(sde.clone());
    assert_eq!(resolve_sde_planet_p0_extraction(&ctx, sys_id, planet_id, None).unwrap_err().code, "UncolonizablePlanetType");

    let mut sess = char_session(90_000_001, 98_000_001);
    assert_eq!(
        call_pi_err(&ctx, &mut sess, "planetMgr", "GetPlanetResourceInfo", &[PyRep::Integer(planet_id)], None).code,
        "UncolonizablePlanetType"
    );

    for (attrs, expected) in [
        (vec![(ATTR_PLANET_RESTRICTION, f64::from(planet_type_id))], "MissingPlanetRawResource"),
        (
            vec![(ATTR_PLANET_RESTRICTION, f64::from(planet_type_id)), (ATTR_HARVESTER_TYPE, 960_099.0)],
            "MissingPinExtractionQuantity",
        ),
        (
            vec![
                (ATTR_PLANET_RESTRICTION, f64::from(planet_type_id)),
                (ATTR_HARVESTER_TYPE, 960_099.0),
                (ATTR_PIN_EXTRACTION_QUANTITY, 1000.0),
            ],
            "MissingPinCycleTime",
        ),
        (
            vec![
                (ATTR_PLANET_RESTRICTION, f64::from(planet_type_id)),
                (ATTR_HARVESTER_TYPE, 960_099.0),
                (ATTR_PIN_EXTRACTION_QUANTITY, 1000.0),
                (ATTR_PIN_CYCLE_TIME, 300.0),
            ],
            "MissingPlanetRawResource",
        ),
    ] {
        sde.types.insert(960_001, make_inv_type(960_001, 1026, attrs.into_iter().collect()));
        ctx.sde = Arc::new(sde.clone());
        assert_eq!(resolve_sde_planet_p0_extraction(&ctx, sys_id, planet_id, Some(960_001)).unwrap_err().code, expected);
    }

    sde.types.insert(960_099, make_inv_type(960_099, 1032, HashMap::new()));
    ctx.sde = Arc::new(sde.clone());
    assert_eq!(resolve_sde_planet_p0_extraction(&ctx, sys_id, planet_id, Some(960_001)).unwrap_err().code, "MissingExportTaxMultiplier");

    sde.types.insert(960_099, make_inv_type(960_099, 1032, HashMap::from([(ATTR_EXPORT_TAX_MULTIPLIER, 5.0)])));
    ctx.sde = Arc::new(sde);
    assert_eq!(resolve_sde_planet_p0_extraction(&ctx, sys_id, planet_id, Some(960_099)).unwrap(), (960_099, 300, 1000, 5.0));

    let invalid_planet = "InvalidPlanetID";
    for m in ["GetPlanetInfo", "GetPlanetResourceInfo", "GetResourceData"] {
        assert_eq!(call_pi_err(&ctx, &mut sess, "planetMgr", m, &[], None).code, invalid_planet);
        assert_eq!(call_pi_err(&ctx, &mut sess, "planetMgr", m, &[PyRep::Integer(-999)], None).code, invalid_planet);
    }

    let bound = BoundObjectEntry {
        bound_id: 1,
        session_id: 1,
        user_id: 1,
        character_id: 90_000_001,
        oid_str: "OID-1".into(),
        ref_id: 1,
        service: "planetMgr".into(),
        object_id: planet_id,
    };
    assert_eq!(
        call_pi_err(&ctx, &mut sess, "planetMgr", "GetResourceData", &[PyRep::Integer(888_888)], Some(&bound)).code,
        "InvalidPlanetResource"
    );

    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(90_000_001).or_default().insert(
            13279,
            TrainedSkillRecord { type_id: 13279, level: 4, skill_points: 50_000.0 },
        );
        snap.planetary_colonies.insert(
            (90_000_001, planet_id),
            PlanetaryColonyRecord {
                owner_id: 90_000_001,
                planet_id,
                solar_system_id: sys_id,
                command_center_level: 3,
                pins_count: 5,
                last_extracted_unix: 1000,
                extracted_commodities: HashMap::from([(960_099, 50_000)]),
            },
        );
    });

    let pinfo = handle(&ctx, &mut sess, "planetMgr", "GetPlanetInfo", &[], &HashMap::new(), Some(&bound)).unwrap().unwrap();
    assert_eq!(pinfo.dict_get("hasColony"), Some(&PyRep::Bool(true)));
    assert_eq!(pinfo.dict_get("commandCenterLevel"), Some(&PyRep::Integer(3)));

    let rdata = handle(
        &ctx,
        &mut sess,
        "planetMgr",
        "GetResourceData",
        &[PyRep::Dict(vec![
            (PyRep::String("resourceTypeID".into()), PyRep::Integer(960_099)),
            (PyRep::String("newBand".into()), PyRep::Integer(2)),
        ])],
        &HashMap::new(),
        Some(&bound),
    )
    .unwrap()
    .unwrap();
    assert_eq!(rdata.dict_get("numBands"), Some(&PyRep::Integer(2)));
}

#[test]
fn industry_job_queries_permissions_and_monitor_branches() {
    let ctx = make_ctx();
    let char_a = 90_000_001_i64;
    let char_b = 90_000_002_i64;
    let corp_id = 98_000_077_i64;
    let station_id = 60_003_760_i64;

    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.insert(
            corp_id,
            CorporationRecord {
                corporation_id: corp_id,
                name: "Ind Corp".into(),
                ticker: "IND".into(),
                ceo_id: char_a,
                hq_station_id: Some(station_id),
                ..Default::default()
            },
        );
        if let Some(ch) = snap.characters.get_mut(&char_a) {
            ch.corporation_id = corp_id;
        }
        if let Some(ch) = snap.characters.get_mut(&char_b) {
            ch.corporation_id = 98_000_999;
        }
        snap.industry_jobs.clear();
        for (jid, act_id) in [(601, 1), (602, 4), (603, 9)] {
            snap.industry_jobs.insert(
                jid,
                IndustryJobRecord {
                    job_id: jid,
                    installer_id: char_a,
                    facility_id: station_id,
                    blueprint_item_id: 9000 + jid,
                    blueprint_type_id: 688,
                    activity_id: act_id,
                    runs: 1,
                    product_type_id: 587,
                    status: "Active".into(),
                    duration_seconds: 600,
                    start_unix: 1000,
                    blueprint_location_id: station_id,
                    output_location_id: station_id,
                    cost: Some(100.0),
                    licensed_runs: None,
                    probability: None,
                    completed_unix: None,
                    completed_character_id: None,
                },
            );
        }
        snap.items.insert(
            9901,
            InventoryItemRecord {
                item_id: 9901,
                type_id: 34,
                owner_id: corp_id,
                location_id: station_id,
                flag: InventoryFlag::CorpSAG1,
                quantity: 500,
                singleton: false,
                ..Default::default()
            },
        );
    });

    let mut sess_a = char_session(char_a, corp_id);
    sess_a.corp_role = corp_roles::ROLE_DIRECTOR;
    let mut sess_b = char_session(char_b, 98_000_999);
    sess_b.corp_role = 0;

    let counts = handle_industry_job_queries(&ctx, &mut sess_a, "industryManager", "GetJobCounts", &[], &HashMap::new()).unwrap().unwrap();
    assert_eq!(counts.dict_get("manufacturingUsed"), Some(&PyRep::Integer(1)));
    assert_eq!(counts.dict_get("researchUsed"), Some(&PyRep::Integer(1)));
    assert_eq!(counts.dict_get("reactionsUsed"), Some(&PyRep::Integer(1)));

    assert_eq!(call_job_err(&ctx, &mut sess_a, "industryManager", "GetJob", &[PyRep::Integer(999_999)]).code, "JobNotFound");
    assert_eq!(call_job_err(&ctx, &mut sess_b, "industryManager", "GetJob", &[PyRep::Integer(601)]).code, "JobOwnershipViolation");
    assert_eq!(call_job_err(&ctx, &mut sess_b, "industryManager", "GetJobsByOwner", &[PyRep::Integer(char_a)]).code, "OwnerAccessDenied");

    let mut gm_sess = sess_b.clone();
    gm_sess.role = service_roles::ROLE_GM_MASK;
    let gm_jobs = handle_industry_job_queries(&ctx, &mut gm_sess, "industryManager", "GetJobsByOwner", &[PyRep::Integer(char_a)], &HashMap::new()).unwrap().unwrap();
    assert!(matches!(gm_jobs, PyRep::List(v) if v.len() == 3));

    assert_eq!(call_job_err(&ctx, &mut sess_a, "industryMonitor", "ConnectJob", &[]).code, "InvalidArguments");
    assert_eq!(call_job_err(&ctx, &mut sess_a, "industryMonitor", "ConnectJob", &[PyRep::String("bad".into())]).code, "InvalidArguments");
    assert_eq!(call_job_err(&ctx, &mut sess_a, "industryMonitor", "ConnectJob", &[PyRep::Integer(999_999)]).code, "JobNotFound");

    let corp_job_kv = PyRep::keyval([
        ("inputLocationID", PyRep::Integer(station_id)),
        ("inputFlagID", PyRep::Integer(InventoryFlag::CorpSAG1 as i64)),
        ("ownerID", PyRep::Integer(corp_id)),
        ("jobID", PyRep::Integer(601)),
    ]);
    assert_eq!(call_job_err(&ctx, &mut sess_b, "industryMonitor", "ConnectJob", std::slice::from_ref(&corp_job_kv)).code, "CrpAccessDenied");

    let conn_ok = handle_industry_job_queries(&ctx, &mut sess_a, "industryMonitor", "ConnectJob", &[corp_job_kv], &HashMap::new()).unwrap().unwrap();
    let mon_id = match conn_ok {
        PyRep::Tuple(ref t) => {
            assert_eq!(t[1], PyRep::Dict(vec![(PyRep::Integer(34), PyRep::Integer(500))]));
            t[0].as_i64().unwrap()
        }
        _ => panic!("expected Tuple"),
    };

    assert_eq!(call_job_err(&ctx, &mut sess_b, "industryMonitor", "DisconnectJob", &[PyRep::Integer(mon_id)]).code, "CrpAccessDenied");
    assert_eq!(call_job_err(&ctx, &mut sess_b, "industryMonitor", "DisconnectJob", &[PyRep::Integer(601)]).code, "CrpAccessDenied");
    assert_eq!(call_job_err(&ctx, &mut sess_a, "industryMonitor", "DisconnectJob", &[PyRep::Integer(999_999)]).code, "JobNotFound");

    let disc_job = handle_industry_job_queries(&ctx, &mut sess_a, "industryMonitor", "DisconnectJob", &[PyRep::Integer(601)], &HashMap::new()).unwrap().unwrap();
    assert_eq!(disc_job.dict_get("unsubscribed"), Some(&PyRep::Bool(true)));
}
