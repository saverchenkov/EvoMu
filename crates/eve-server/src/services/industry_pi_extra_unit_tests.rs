//! Extra branch-coverage and mutation-killing unit tests for `industry_pi`.

use crate::services::industry_pi::*;
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
            { let mut __corp = CorporationRecord::default();
                __corp.corporation_id = corp_id;
                __corp.name = "Ind Corp".into();
                __corp.ticker = "IND".into();
                __corp.ceo_id = char_a;
                __corp.hq_station_id = Some(station_id);
                __corp },
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

    // Calling DisconnectJob(None) or with no args (industryWnd.Close when jobData is None) cleanly returns None
    let disc_none = handle_industry_job_queries(&ctx, &mut sess_a, "industryMonitor", "DisconnectJob", &[PyRep::None], &HashMap::new()).unwrap().unwrap();
    assert_eq!(disc_none, PyRep::None);
    let disc_empty = handle_industry_job_queries(&ctx, &mut sess_a, "industryMonitor", "DisconnectJob", &[], &HashMap::new()).unwrap().unwrap();
    assert_eq!(disc_empty, PyRep::None);
}

#[test]
fn format_industry_job_pyrep_at_promotes_elapsed_and_aligns_advancetime_dates() {
    let to_ft = |u: u64| ((u as i64).saturating_add(11_644_473_600)).saturating_mul(10_000_000);
    let offset_7d = 7 * 86_400_u64;
    let client_base = 1_800_000_000_u64;
    let job = IndustryJobRecord {
        job_id: 7701,
        installer_id: 90_000_001,
        facility_id: 60_003_760,
        blueprint_item_id: 88_001,
        blueprint_type_id: 967,
        activity_id: 1,
        runs: 1,
        product_type_id: 587,
        status: "Active".into(),
        duration_seconds: 600,
        start_unix: client_base + offset_7d,
        blueprint_location_id: 60_003_760,
        output_location_id: 60_003_760,
        cost: Some(250.0),
        licensed_runs: None,
        probability: None,
        completed_unix: None,
        completed_character_id: None,
    };
    let meta = IndustryJobCostMeta::default();

    // Unelapsed job keeps "Active" while FILETIME dates subtract `/advancetime` lead (`offset_7d`)
    // so client `blue.os.GetWallclockTime()` sees a 600s job rather than 7d + 600s (#1052).
    let before = format_industry_job_pyrep_at(
        &job,
        &meta,
        Some(client_base + offset_7d + 599),
        offset_7d,
    );
    assert_eq!(before.dict_get("status"), Some(&PyRep::String("Active".into())));
    assert_eq!(before.dict_get("startUnix"), Some(&PyRep::Integer((client_base + offset_7d) as i64)));
    assert_eq!(before.dict_get("endUnix"), Some(&PyRep::Integer((client_base + offset_7d + 600) as i64)));
    assert_eq!(before.dict_get("startDate"), Some(&PyRep::Integer(to_ft(client_base))));
    assert_eq!(before.dict_get("endDate"), Some(&PyRep::Integer(to_ft(client_base + 600))));

    // Elapsed job promotes to "Ready" on wire while preserving aligned dates.
    let at_end = format_industry_job_pyrep_at(
        &job,
        &meta,
        Some(client_base + offset_7d + 600),
        offset_7d,
    );
    assert_eq!(at_end.dict_get("status"), Some(&PyRep::String("Ready".into())));
    assert_eq!(at_end.dict_get("endDate"), Some(&PyRep::Integer(to_ft(client_base + 600))));

    // Paused job paused at or past end_unix promotes to "Ready" with aligned pauseDate.
    let mut paused_job = job.clone();
    paused_job.status = "Paused".into();
    let paused_meta = IndustryJobCostMeta {
        paused_at_unix: Some(client_base + offset_7d + 600),
        ..Default::default()
    };
    let paused_ready = format_industry_job_pyrep_at(
        &paused_job,
        &paused_meta,
        Some(client_base + offset_7d + 700),
        offset_7d,
    );
    assert_eq!(paused_ready.dict_get("status"), Some(&PyRep::String("Ready".into())));
    assert_eq!(paused_ready.dict_get("pauseDate"), Some(&PyRep::Integer(to_ft(client_base + 600))));

    // GetJobCounts must continue counting "Ready" undelivered jobs against open slot totals.
    let ctx = make_ctx();
    let char_id = 90_000_001_i64;
    let corp_id = 98_000_001_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.remove(&char_id);
        snap.industry_jobs.clear();
        let mut ready_mfg = job.clone();
        ready_mfg.status = "Ready".into();
        snap.industry_jobs.insert(ready_mfg.job_id, ready_mfg);
        let mut ready_sci = job.clone();
        ready_sci.job_id = 7702;
        ready_sci.activity_id = 4;
        ready_sci.status = "Ready".into();
        snap.industry_jobs.insert(ready_sci.job_id, ready_sci);
        let mut ready_react = job;
        ready_react.job_id = 7703;
        ready_react.activity_id = 9;
        ready_react.status = "Ready".into();
        snap.industry_jobs.insert(ready_react.job_id, ready_react);
    });
    let mut sess = char_session(char_id, corp_id);
    let counts = handle_industry_job_queries(
        &ctx,
        &mut sess,
        "industryManager",
        "GetJobCounts",
        &[],
        &HashMap::new(),
    )
    .expect("GetJobCounts dispatch")
    .expect("GetJobCounts ok");
    assert_eq!(counts.dict_get("manufacturingUsed"), Some(&PyRep::Integer(1)));
    assert_eq!(counts.dict_get("researchUsed"), Some(&PyRep::Integer(1)));
    assert_eq!(counts.dict_get("reactionsUsed"), Some(&PyRep::Integer(1)));
}

#[test]
fn industry_manager_corp_rbac_db_first_and_cross_owner_isolation() {
    let ctx = make_ctx();
    let now_unix = ctx.now_unix();
    let corp_a = 98_100_001_i64;
    let corp_b = 98_100_002_i64;
    let npc_corp = 1_000_170_i64;
    let uninserted_corp = 98_100_888_i64;

    let char_ceo = 91_100_001_i64;
    let char_factory_mgr = 91_100_002_i64;
    let char_director = 91_100_003_i64;
    let char_regular = 91_100_004_i64;
    let char_station_mgr = 91_100_005_i64;
    let char_non_member = 91_100_006_i64;
    let char_role_dropped = 91_100_007_i64;
    let char_ex_installer = 91_100_008_i64;
    let char_npc = 91_100_009_i64;
    let char_uninserted = 91_100_010_i64;

    let facility_id = 1_000_000_199_001_i64;
    let personal_job_id = 900_101_i64;
    let corp_job_id = 900_102_i64;
    let ex_installer_corp_job_id = 900_103_i64;
    let ex_installer_personal_job_id = 900_104_i64;
    let npc_corp_job_id = 900_105_i64;

    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            facility_id,
            eve_db::UpwellStructureRecord {
                structure_id: facility_id,
                type_id: 35832,
                owner_corp_id: corp_a,
                solar_system_id: 30_000_142,
                name: "RBAC Foundry".into(),
                position: [0.0; 3],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 100,
                ..Default::default()
            },
        );
        snap.items.insert(
            99_900_101,
            eve_db::InventoryItemRecord {
                item_id: 99_900_101,
                type_id: 35878,
                owner_id: corp_a,
                location_id: facility_id,
                flag: eve_db::InventoryFlag::ServiceSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );

        let mut corp_a_rec = eve_db::CorporationRecord::new(corp_a, "Alpha Foundry", "ALFA", char_ceo);
        corp_a_rec.members = std::collections::HashSet::from([
            char_ceo,
            char_factory_mgr,
            char_director,
            char_regular,
            char_station_mgr,
            char_role_dropped,
        ]);
        corp_a_rec.member_roles = HashMap::from([
            (char_factory_mgr, eve_db::corp_roles::ROLE_FACTORY_MANAGER),
            (char_director, eve_db::corp_roles::ROLE_DIRECTOR),
            (char_regular, 0),
            (
                char_station_mgr,
                eve_db::corp_roles::ROLE_STATION_MANAGER | eve_db::corp_roles::ROLE_ACCOUNTANT,
            ),
            (char_role_dropped, 0),
            // Even if stale DB role row remains, non-membership must deny access.
            (char_non_member, eve_db::corp_roles::ROLE_FACTORY_MANAGER),
        ]);
        snap.corporations.insert(corp_a, corp_a_rec);

        let mut corp_b_rec =
            eve_db::CorporationRecord::new(corp_b, "Beta Syndicate", "BETA", char_ex_installer);
        corp_b_rec.members = std::collections::HashSet::from([char_ex_installer]);
        corp_b_rec.member_roles =
            HashMap::from([(char_ex_installer, eve_db::corp_roles::ROLE_DIRECTOR)]);
        snap.corporations.insert(corp_b, corp_b_rec);

        for (cid, corpid, name) in [
            (char_ceo, corp_a, "CEO"),
            (char_factory_mgr, corp_a, "FactoryMgr"),
            (char_director, corp_a, "Director"),
            (char_regular, corp_a, "Regular"),
            (char_station_mgr, corp_a, "StationMgr"),
            (char_non_member, corp_a, "NonMember"),
            (char_role_dropped, corp_a, "RoleDropped"),
            (char_ex_installer, corp_b, "ExInstaller"),
            (char_npc, npc_corp, "NpcPilot"),
            (char_uninserted, uninserted_corp, "UninsertedPilot"),
        ] {
            snap.characters.insert(
                cid,
                eve_db::CharacterRecord {
                    character_id: cid,
                    corporation_id: corpid,
                    name: name.into(),
                    ..Default::default()
                },
            );
        }

        let base_job = |job_id: i64, installer_id: i64| eve_db::IndustryJobRecord {
            job_id,
            installer_id,
            facility_id,
            blueprint_item_id: job_id + 10_000,
            blueprint_type_id: 587,
            activity_id: 1,
            runs: 1,
            product_type_id: 587,
            status: "Active".into(),
            duration_seconds: 3_600,
            start_unix: now_unix.saturating_sub(60),
            blueprint_location_id: facility_id,
            output_location_id: facility_id,
            ..Default::default()
        };

        snap.industry_jobs
            .insert(personal_job_id, base_job(personal_job_id, char_regular));
        save_job_cost_meta(
            snap,
            personal_job_id,
            &IndustryJobCostMeta {
                use_corp: false,
                owner_id: char_regular,
                ..Default::default()
            },
        );

        snap.industry_jobs
            .insert(corp_job_id, base_job(corp_job_id, char_factory_mgr));
        save_job_cost_meta(
            snap,
            corp_job_id,
            &IndustryJobCostMeta {
                use_corp: true,
                owner_id: corp_a,
                ..Default::default()
            },
        );

        snap.industry_jobs.insert(
            ex_installer_corp_job_id,
            base_job(ex_installer_corp_job_id, char_ex_installer),
        );
        save_job_cost_meta(
            snap,
            ex_installer_corp_job_id,
            &IndustryJobCostMeta {
                use_corp: true,
                owner_id: corp_a,
                ..Default::default()
            },
        );

        snap.industry_jobs.insert(
            ex_installer_personal_job_id,
            base_job(ex_installer_personal_job_id, char_ex_installer),
        );
        save_job_cost_meta(
            snap,
            ex_installer_personal_job_id,
            &IndustryJobCostMeta {
                use_corp: false,
                owner_id: char_ex_installer,
                ..Default::default()
            },
        );

        snap.industry_jobs
            .insert(npc_corp_job_id, base_job(npc_corp_job_id, char_npc));
        save_job_cost_meta(
            snap,
            npc_corp_job_id,
            &IndustryJobCostMeta {
                use_corp: true,
                owner_id: npc_corp,
                ..Default::default()
            },
        );
    });

    // Personal job: owner allowed, cross-char denied.
    let mut s_reg = char_session(char_regular, corp_a);
    let own_job = handle_industry_job_queries(
        &ctx,
        &mut s_reg,
        "industryManager",
        "GetJob",
        &[PyRep::Integer(personal_job_id)],
        &HashMap::new(),
    )
    .expect("GetJob handled")
    .expect("personal owner allowed");
    assert_eq!(own_job.dict_get("jobID").and_then(PyRep::as_i64), Some(personal_job_id));

    let own_list = handle_industry_job_queries(
        &ctx,
        &mut s_reg,
        "industryManager",
        "GetJobsByOwner",
        &[PyRep::Integer(char_regular)],
        &HashMap::new(),
    )
    .expect("GetJobsByOwner handled")
    .expect("personal list ok");
    let PyRep::List(own_rows) = own_list else {
        panic!("expected list for personal jobs");
    };
    assert_eq!(own_rows.len(), 1);
    assert_eq!(
        own_rows[0].dict_get("jobID").and_then(PyRep::as_i64),
        Some(personal_job_id)
    );

    let mut s_mgr = char_session(char_factory_mgr, corp_a);
    assert_eq!(
        call_job_err(&ctx, &mut s_mgr, "industryManager", "GetJob", &[PyRep::Integer(personal_job_id)]).code,
        "JobOwnershipViolation"
    );
    assert_eq!(
        call_job_err(
            &ctx,
            &mut s_mgr,
            "industryManager",
            "GetJobsByOwner",
            &[PyRep::Integer(char_regular)],
        )
        .code,
        "OwnerAccessDenied"
    );

    // Same-corp member without roles & unrelated roles: denied on both GetJob and GetJobsByOwner(corp_a).
    assert_eq!(
        call_job_err(&ctx, &mut s_reg, "industryManager", "GetJob", &[PyRep::Integer(corp_job_id)]).code,
        "JobOwnershipViolation"
    );
    assert_eq!(
        call_job_err(
            &ctx,
            &mut s_reg,
            "industryManager",
            "GetJobsByOwner",
            &[PyRep::Integer(corp_a)],
        )
        .code,
        "OwnerAccessDenied"
    );

    let mut s_station = char_session(char_station_mgr, corp_a);
    s_station.corp_role =
        eve_db::corp_roles::ROLE_STATION_MANAGER | eve_db::corp_roles::ROLE_ACCOUNTANT;
    assert_eq!(
        call_job_err(&ctx, &mut s_station, "industryManager", "GetJob", &[PyRep::Integer(corp_job_id)]).code,
        "JobOwnershipViolation"
    );
    assert_eq!(
        call_job_err(
            &ctx,
            &mut s_station,
            "industryManager",
            "GetJobsByOwner",
            &[PyRep::Integer(corp_a)],
        )
        .code,
        "OwnerAccessDenied"
    );

    // ROLE_FACTORY_MANAGER, ROLE_DIRECTOR, and CEO: allowed on both GetJob and GetJobsByOwner(corp_a).
    let mut s_ceo = char_session(char_ceo, corp_a);
    let mut s_dir = char_session(char_director, corp_a);
    for sess in [&mut s_mgr, &mut s_dir, &mut s_ceo] {
        let rep = handle_industry_job_queries(
            &ctx,
            sess,
            "industryManager",
            "GetJob",
            &[PyRep::Integer(corp_job_id)],
            &HashMap::new(),
        )
        .expect("GetJob handled")
        .expect("authorized corp role allowed");
        assert_eq!(rep.dict_get("jobID").and_then(PyRep::as_i64), Some(corp_job_id));

        let list = handle_industry_job_queries(
            &ctx,
            sess,
            "industryManager",
            "GetJobsByOwner",
            &[PyRep::Integer(corp_a)],
            &HashMap::new(),
        )
        .expect("GetJobsByOwner handled")
        .expect("authorized corp list allowed");
        let PyRep::List(corp_rows) = list else {
            panic!("expected list for corp jobs");
        };
        assert_eq!(corp_rows.len(), 2);
        let mut got_ids: Vec<i64> = corp_rows
            .iter()
            .filter_map(|r| r.dict_get("jobID").and_then(PyRep::as_i64))
            .collect();
        got_ids.sort_unstable();
        assert_eq!(got_ids, vec![corp_job_id, ex_installer_corp_job_id]);
    }

    // DB-first corp check defeats spoofed session.corporation_id + stale session.corp_role.
    let mut s_ex = char_session(char_ex_installer, corp_a);
    s_ex.corp_role = eve_db::corp_roles::ROLE_DIRECTOR;
    assert_eq!(
        call_job_err(&ctx, &mut s_ex, "industryManager", "GetJob", &[PyRep::Integer(ex_installer_corp_job_id)]).code,
        "JobOwnershipViolation"
    );
    assert_eq!(
        call_job_err(&ctx, &mut s_ex, "industryManager", "GetJobsByOwner", &[PyRep::Integer(corp_a)]).code,
        "OwnerAccessDenied"
    );
    // Personal query for ex-installer returns only their personal job, never their old corp job.
    let ex_personal = handle_industry_job_queries(
        &ctx,
        &mut s_ex,
        "industryManager",
        "GetJobsByOwner",
        &[PyRep::Integer(char_ex_installer)],
        &HashMap::new(),
    )
    .expect("GetJobsByOwner handled")
    .expect("ex-installer personal list ok");
    let PyRep::List(ex_rows) = ex_personal else {
        panic!("expected list for ex-installer personal jobs");
    };
    assert_eq!(ex_rows.len(), 1);
    assert_eq!(
        ex_rows[0].dict_get("jobID").and_then(PyRep::as_i64),
        Some(ex_installer_personal_job_id)
    );

    // Foreign-corp director is denied on corp_a jobs.
    let mut s_ex_real = char_session(char_ex_installer, corp_b);
    s_ex_real.corp_role = eve_db::corp_roles::ROLE_DIRECTOR;
    assert_eq!(
        call_job_err(&ctx, &mut s_ex_real, "industryManager", "GetJob", &[PyRep::Integer(corp_job_id)]).code,
        "JobOwnershipViolation"
    );
    assert_eq!(
        call_job_err(&ctx, &mut s_ex_real, "industryManager", "GetJobsByOwner", &[PyRep::Integer(corp_a)]).code,
        "OwnerAccessDenied"
    );

    // Character not present in corp_a.members is denied even if character/member_roles point at corp_a.
    let mut s_non_member = char_session(char_non_member, corp_a);
    s_non_member.corp_role = eve_db::corp_roles::ROLE_DIRECTOR;
    assert_eq!(
        call_job_err(&ctx, &mut s_non_member, "industryManager", "GetJob", &[PyRep::Integer(corp_job_id)]).code,
        "JobOwnershipViolation"
    );
    assert_eq!(
        call_job_err(&ctx, &mut s_non_member, "industryManager", "GetJobsByOwner", &[PyRep::Integer(corp_a)]).code,
        "OwnerAccessDenied"
    );

    // Member with DB roles == 0 and stale session.corp_role = ROLE_DIRECTOR is denied.
    let mut s_dropped = char_session(char_role_dropped, corp_a);
    s_dropped.corp_role = eve_db::corp_roles::ROLE_DIRECTOR;
    assert_eq!(
        call_job_err(&ctx, &mut s_dropped, "industryManager", "GetJob", &[PyRep::Integer(corp_job_id)]).code,
        "JobOwnershipViolation"
    );
    assert_eq!(
        call_job_err(&ctx, &mut s_dropped, "industryManager", "GetJobsByOwner", &[PyRep::Integer(corp_a)]).code,
        "OwnerAccessDenied"
    );

    // NPC corporation and uninserted player corporation are denied cleanly.
    let mut s_npc = char_session(char_npc, npc_corp);
    s_npc.corp_role = eve_db::corp_roles::ROLE_DIRECTOR;
    assert_eq!(
        call_job_err(&ctx, &mut s_npc, "industryManager", "GetJob", &[PyRep::Integer(npc_corp_job_id)]).code,
        "JobOwnershipViolation"
    );
    assert_eq!(
        call_job_err(&ctx, &mut s_npc, "industryManager", "GetJobsByOwner", &[PyRep::Integer(npc_corp)]).code,
        "OwnerAccessDenied"
    );
    assert_eq!(
        call_job_err(&ctx, &mut s_npc, "industryManager", "GetJobsByOwner", &[PyRep::Integer(0)]).code,
        "OwnerAccessDenied"
    );

    let mut s_uninserted = char_session(char_uninserted, uninserted_corp);
    s_uninserted.corp_role = eve_db::corp_roles::ROLE_DIRECTOR;
    assert_eq!(
        call_job_err(
            &ctx,
            &mut s_uninserted,
            "industryManager",
            "GetJobsByOwner",
            &[PyRep::Integer(uninserted_corp)],
        )
        .code,
        "OwnerAccessDenied"
    );
}

#[test]
fn industry_manager_get_job_get_jobs_by_owner_and_counts_are_read_pure() {
    let ctx = make_ctx();
    let now_unix = ctx.now_unix();
    let owner_char = 91_100_021_i64;
    let outsider_char = 91_100_022_i64;
    let corp_id = 98_100_021_i64;

    let offline_citadel = 1_000_000_299_001_i64;
    let online_citadel = 1_000_000_299_002_i64;
    let active_offline_job_id = 900_201_i64;
    let paused_online_job_id = 900_202_i64;
    let corp_job_id = 900_203_i64;

    ctx.db.with_mut_snapshot(|snap| {
        let mut corp = eve_db::CorporationRecord::new(corp_id, "Pure Corp", "PURE", owner_char);
        corp.members = std::collections::HashSet::from([owner_char, outsider_char]);
        corp.member_roles = HashMap::from([(outsider_char, 0)]);
        snap.corporations.insert(corp_id, corp);

        for (cid, name) in [(owner_char, "Owner"), (outsider_char, "Outsider")] {
            snap.characters.insert(
                cid,
                eve_db::CharacterRecord {
                    character_id: cid,
                    corporation_id: corp_id,
                    name: name.into(),
                    ..Default::default()
                },
            );
        }

        // Offline structure (zero fuel) holding an Active job -> projects Paused without mutating DB.
        snap.structures.insert(
            offline_citadel,
            eve_db::UpwellStructureRecord {
                structure_id: offline_citadel,
                type_id: 35832,
                owner_corp_id: corp_id,
                solar_system_id: 30_000_142,
                name: "Offline Citadel".into(),
                position: [0.0; 3],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 0,
                ..Default::default()
            },
        );
        // Online structure (fueled + Standup Manufacturing Plant I) holding a Paused job -> projects Active.
        snap.structures.insert(
            online_citadel,
            eve_db::UpwellStructureRecord {
                structure_id: online_citadel,
                type_id: 35832,
                owner_corp_id: corp_id,
                solar_system_id: 30_000_142,
                name: "Online Citadel".into(),
                position: [1_000.0, 0.0, 0.0],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 100,
                ..Default::default()
            },
        );
        snap.items.insert(
            99_900_201,
            eve_db::InventoryItemRecord {
                item_id: 99_900_201,
                type_id: 35878,
                owner_id: corp_id,
                location_id: online_citadel,
                flag: eve_db::InventoryFlag::ServiceSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );

        let start_unix = now_unix.saturating_sub(300);
        snap.industry_jobs.insert(
            active_offline_job_id,
            eve_db::IndustryJobRecord {
                job_id: active_offline_job_id,
                installer_id: owner_char,
                facility_id: offline_citadel,
                blueprint_item_id: 800_201,
                blueprint_type_id: 587,
                activity_id: 1,
                runs: 1,
                product_type_id: 587,
                status: "Active".into(),
                duration_seconds: 3_600,
                start_unix,
                blueprint_location_id: offline_citadel,
                output_location_id: offline_citadel,
                ..Default::default()
            },
        );
        save_job_cost_meta(
            snap,
            active_offline_job_id,
            &IndustryJobCostMeta {
                use_corp: false,
                owner_id: owner_char,
                ..Default::default()
            },
        );

        snap.industry_jobs.insert(
            paused_online_job_id,
            eve_db::IndustryJobRecord {
                job_id: paused_online_job_id,
                installer_id: owner_char,
                facility_id: online_citadel,
                blueprint_item_id: 800_202,
                blueprint_type_id: 587,
                activity_id: 1,
                runs: 1,
                product_type_id: 587,
                status: "Paused".into(),
                duration_seconds: 3_600,
                start_unix,
                blueprint_location_id: online_citadel,
                output_location_id: online_citadel,
                ..Default::default()
            },
        );
        save_job_cost_meta(
            snap,
            paused_online_job_id,
            &IndustryJobCostMeta {
                use_corp: false,
                owner_id: owner_char,
                paused_at_unix: Some(now_unix.saturating_sub(120)),
                ..Default::default()
            },
        );

        snap.industry_jobs.insert(
            corp_job_id,
            eve_db::IndustryJobRecord {
                job_id: corp_job_id,
                installer_id: owner_char,
                facility_id: online_citadel,
                blueprint_item_id: 800_203,
                blueprint_type_id: 587,
                activity_id: 1,
                runs: 1,
                product_type_id: 587,
                status: "Active".into(),
                duration_seconds: 3_600,
                start_unix,
                blueprint_location_id: online_citadel,
                output_location_id: online_citadel,
                ..Default::default()
            },
        );
        save_job_cost_meta(
            snap,
            corp_job_id,
            &IndustryJobCostMeta {
                use_corp: true,
                owner_id: corp_id,
                ..Default::default()
            },
        );
    });

    let snapshot_serialized = || {
        ctx.db.with_snapshot(|s| {
            let jobs: std::collections::BTreeMap<i64, _> =
                s.industry_jobs.iter().map(|(&k, v)| (k, v.clone())).collect();
            let notes: std::collections::BTreeMap<i64, _> =
                s.owner_notes.iter().map(|(&k, v)| (k, v.clone())).collect();
            let serialized = serde_json::to_string(&(&jobs, &notes)).expect("serialize state");
            (s.clone(), serialized)
        })
    };

    let (snap_before, serialized_before) = snapshot_serialized();

    let mut s_owner = char_session(owner_char, corp_id);
    let mut s_outsider = char_session(outsider_char, corp_id);

    // Success reads project pause/resume status without mutating DB.
    let rep_offline = handle_industry_job_queries(
        &ctx,
        &mut s_owner,
        "industryManager",
        "GetJob",
        &[PyRep::Integer(active_offline_job_id)],
        &HashMap::new(),
    )
    .expect("GetJob handled")
    .expect("GetJob offline ok");
    assert_eq!(rep_offline.dict_get("status"), Some(&PyRep::String("Paused".into())));

    let rep_online = handle_industry_job_queries(
        &ctx,
        &mut s_owner,
        "industryManager",
        "GetJob",
        &[PyRep::Integer(paused_online_job_id)],
        &HashMap::new(),
    )
    .expect("GetJob handled")
    .expect("GetJob online ok");
    assert_eq!(rep_online.dict_get("status"), Some(&PyRep::String("Active".into())));
    assert!(
        rep_online
            .dict_get("durationSeconds")
            .and_then(PyRep::as_i64)
            .unwrap_or(0)
            >= 3_600
    );

    let owner_list = handle_industry_job_queries(
        &ctx,
        &mut s_owner,
        "industryManager",
        "GetJobsByOwner",
        &[PyRep::Integer(owner_char)],
        &HashMap::new(),
    )
    .expect("GetJobsByOwner handled")
    .expect("GetJobsByOwner ok");
    let PyRep::List(rows) = owner_list else {
        panic!("expected list from GetJobsByOwner");
    };
    assert_eq!(rows.len(), 2);
    let statuses: std::collections::HashMap<i64, String> = rows
        .iter()
        .filter_map(|r| {
            let jid = r.dict_get("jobID").and_then(PyRep::as_i64)?;
            let st = match r.dict_get("status") {
                Some(PyRep::String(s)) => s.clone(),
                _ => return None,
            };
            Some((jid, st))
        })
        .collect();
    assert_eq!(statuses.get(&active_offline_job_id).map(String::as_str), Some("Paused"));
    assert_eq!(statuses.get(&paused_online_job_id).map(String::as_str), Some("Active"));

    let counts = handle_industry_job_queries(
        &ctx,
        &mut s_owner,
        "industryManager",
        "GetJobCounts",
        &[],
        &HashMap::new(),
    )
    .expect("GetJobCounts handled")
    .expect("GetJobCounts ok");
    assert_eq!(counts.dict_get("manufacturingUsed"), Some(&PyRep::Integer(3)));

    // Error reads also perform zero DB writes.
    assert_eq!(
        call_job_err(&ctx, &mut s_outsider, "industryManager", "GetJob", &[PyRep::Integer(corp_job_id)]).code,
        "JobOwnershipViolation"
    );
    assert_eq!(
        call_job_err(&ctx, &mut s_outsider, "industryManager", "GetJob", &[PyRep::Integer(999_999)]).code,
        "JobNotFound"
    );
    assert_eq!(
        call_job_err(
            &ctx,
            &mut s_outsider,
            "industryManager",
            "GetJobsByOwner",
            &[PyRep::Integer(corp_id)],
        )
        .code,
        "OwnerAccessDenied"
    );

    let (snap_after, serialized_after) = snapshot_serialized();
    assert_eq!(snap_before, snap_after);
    assert_eq!(serialized_before, serialized_after);
}
