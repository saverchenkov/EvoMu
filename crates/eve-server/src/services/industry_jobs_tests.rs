use super::*;
use crate::router::ServerContext;
use crate::services::industry_pi::{ATTR_BLUEPRINT_ME, ATTR_BLUEPRINT_TE};
use eve_db::{InventoryFlag, InventoryItemRecord, IskCents, TrainedSkillRecord, WorldRepository};
use eve_proto::PyRep;
use eve_sde::{
    BlueprintActivity, BlueprintInfo, BlueprintMaterial, BlueprintProduct, InvType, SdeRegistry,
};
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

fn make_inv_type(type_id: u32, group_id: u32, name: &str, volume: f64, base_price: f64) -> InvType {
    InvType {
        type_id,
        group_id,
        category_id: 6,
        name: name.into(),
        mass: 1.0,
        volume,
        capacity: 0.0,
        radius: 10.0,
        base_price,
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
        default_attributes: HashMap::new(),
    }
}

fn make_item(
    item_id: i64,
    type_id: u32,
    owner_id: i64,
    location_id: i64,
    flag: InventoryFlag,
    quantity: i32,
    singleton: bool,
) -> InventoryItemRecord {
    InventoryItemRecord {
        item_id,
        type_id,
        owner_id,
        location_id,
        flag,
        quantity,
        singleton,
        ..Default::default()
    }
}

fn setup_industry_sde(ctx: &mut ServerContext) {
    let mut sde = (*ctx.sde).clone();
    sde.types
        .insert(34, make_inv_type(34, 18, "Tritanium", 0.01, 2.0));
    sde.types
        .insert(587, make_inv_type(587, 25, "Rifter", 27289.0, 250_000.0));
    sde.types.insert(
        688,
        make_inv_type(688, 105, "Rifter Blueprint", 0.01, 1_000_000.0),
    );
    sde.blueprints.insert(
        688,
        BlueprintInfo {
            blueprint_type_id: 688,
            product_type_id: 587,
            product_quantity: 1,
            max_production_limit: 10,
            manufacturing_time_sec: 600,
            research_me_time_sec: 105,
            research_te_time_sec: 105,
            copy_time_sec: 100,
            invention_time_sec: 200,
            invention_base_probability: 1.0,
            materials: vec![BlueprintMaterial {
                type_id: 34,
                quantity: 1000,
            }],
            activities: HashMap::from([
                (
                    "manufacturing".into(),
                    BlueprintActivity {
                        time: 600,
                        materials: vec![BlueprintMaterial {
                            type_id: 34,
                            quantity: 1000,
                        }],
                        products: vec![BlueprintProduct {
                            type_id: 587,
                            quantity: 1,
                            probability: None,
                        }],
                        skills: vec![],
                    },
                ),
                (
                    "invention".into(),
                    BlueprintActivity {
                        time: 200,
                        materials: vec![BlueprintMaterial {
                            type_id: 20424,
                            quantity: 2,
                        }],
                        products: vec![BlueprintProduct {
                            type_id: 11372,
                            quantity: 1,
                            probability: None,
                        }],
                        skills: vec![],
                    },
                ),
            ]),
        },
    );
    ctx.sde = Arc::new(sde);
}

#[test]
fn install_job_validation_errors_are_enforced() {
    let mut ctx = make_test_context();
    setup_industry_sde(&mut ctx);
    let char_id = 90_000_001;
    let station_id = 60_003_760;
    let mut session = char_session(char_id, 98_000_001);
    session.station_id = None;
    session.structure_id = None;

    let err_undocked = handle_industry_jobs(
        &ctx,
        &mut session,
        "industryManager",
        "InstallJob",
        &[PyRep::Integer(688), PyRep::Integer(1), PyRep::Integer(1)],
        &HashMap::new(),
    )
    .unwrap()
    .unwrap_err();
    assert_eq!(err_undocked.code, "NotDocked");

    session.station_id = Some(station_id);
    session.solar_system_id = Some(30_000_142);

    let err_runs = handle_industry_jobs(
        &ctx,
        &mut session,
        "industryManager",
        "InstallJob",
        &[PyRep::Integer(688), PyRep::Integer(1), PyRep::Integer(0)],
        &HashMap::new(),
    )
    .unwrap()
    .unwrap_err();
    assert_eq!(err_runs.code, "InvalidIndustryJobParameters");

    let err_act = handle_industry_jobs(
        &ctx,
        &mut session,
        "industryManager",
        "InstallJob",
        &[PyRep::Integer(688), PyRep::Integer(2), PyRep::Integer(1)],
        &HashMap::new(),
    )
    .unwrap()
    .unwrap_err();
    assert_eq!(err_act.code, "InvalidIndustryJobParameters");

    let err_unknown = handle_industry_jobs(
        &ctx,
        &mut session,
        "industryManager",
        "InstallJob",
        &[PyRep::Integer(999999), PyRep::Integer(1), PyRep::Integer(1)],
        &HashMap::new(),
    )
    .unwrap()
    .unwrap_err();
    assert_eq!(err_unknown.code, "UnknownBlueprint");

    let err_no_bp = handle_industry_jobs(
        &ctx,
        &mut session,
        "industryManager",
        "InstallJob",
        &[PyRep::Integer(688), PyRep::Integer(1), PyRep::Integer(1)],
        &HashMap::new(),
    )
    .unwrap()
    .unwrap_err();
    assert_eq!(err_no_bp.code, "BlueprintNotAvailable");

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            10_001,
            make_item(
                10_001,
                688,
                char_id,
                station_id,
                InventoryFlag::Hangar,
                1,
                true,
            ),
        );
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.balance = IskCents::from_f64_isk(1_000_000_000.0);
        }
        snap.skills.remove(&char_id);
    });

    let err_no_skill = handle_industry_jobs(
        &ctx,
        &mut session,
        "industryManager",
        "InstallJob",
        &[PyRep::Integer(688), PyRep::Integer(1), PyRep::Integer(1)],
        &HashMap::new(),
    )
    .unwrap()
    .unwrap_err();
    assert_eq!(err_no_skill.code, "MissingIndustrySkills");

    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(char_id).or_default().insert(
            3380,
            TrainedSkillRecord {
                type_id: 3380,
                level: 5,
                skill_points: 256_000.0,
            },
        );
        snap.items.retain(|_, i| i.type_id != 34);
    });

    let err_no_mat = handle_industry_jobs(
        &ctx,
        &mut session,
        "industryManager",
        "InstallJob",
        &[PyRep::Integer(688), PyRep::Integer(1), PyRep::Integer(1)],
        &HashMap::new(),
    )
    .unwrap()
    .unwrap_err();
    assert_eq!(err_no_mat.code, "InsufficientManufacturingMaterials");
}

#[test]
fn install_cancel_and_complete_manufacturing_job_lifecycle() {
    let mut ctx = make_test_context();
    setup_industry_sde(&mut ctx);
    let char_id = 90_000_001;
    let station_id = 60_003_760;
    let mut session = char_session(char_id, 98_000_001);
    session.station_id = Some(station_id);
    session.solar_system_id = Some(30_000_142);

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.balance = IskCents::from_f64_isk(1_000_000_000.0);
        }
        snap.skills.remove(&char_id);
        snap.skills.entry(char_id).or_default().insert(
            3380,
            TrainedSkillRecord {
                type_id: 3380,
                level: 5,
                skill_points: 256_000.0,
            },
        );
        snap.items.retain(|_, i| i.type_id != 34 && i.type_id != 688);
        snap.items.insert(
            10_001,
            make_item(
                10_001,
                688,
                char_id,
                station_id,
                InventoryFlag::Hangar,
                1,
                true,
            ),
        );
        snap.items.insert(
            10_002,
            make_item(
                10_002,
                34,
                char_id,
                station_id,
                InventoryFlag::Hangar,
                5_000,
                false,
            ),
        );
    });

    let install_rep = handle_industry_jobs(
        &ctx,
        &mut session,
        "industryManager",
        "InstallJob",
        &[PyRep::keyval([
            ("blueprintItemID", PyRep::Integer(10_001)),
            ("blueprintTypeID", PyRep::Integer(688)),
            ("activityID", PyRep::Integer(1)),
            ("runs", PyRep::Integer(2)),
            ("facilityID", PyRep::Integer(station_id)),
        ])],
        &HashMap::new(),
    )
    .unwrap()
    .unwrap();
    let job_id = install_rep.dict_get("jobID").unwrap().as_i64().unwrap();
    assert!(job_id > 0);

    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items[&10_001].flag, InventoryFlag::Locked);
        assert_eq!(snap.items[&10_002].quantity, 3_000);
    });

    let err_slot_full = handle_industry_jobs(
        &ctx,
        &mut session,
        "industryManager",
        "InstallJob",
        &[PyRep::Integer(688), PyRep::Integer(1), PyRep::Integer(1)],
        &HashMap::new(),
    )
    .unwrap()
    .unwrap_err();
    assert_eq!(err_slot_full.code, "IndustrySlotsFull");

    let cancel_rep = handle_industry_jobs(
        &ctx,
        &mut session,
        "industryManager",
        "CancelJob",
        &[PyRep::Integer(job_id)],
        &HashMap::new(),
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        cancel_rep.dict_get("status").unwrap().as_str(),
        Some("Cancelled")
    );
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items[&10_001].flag, InventoryFlag::Hangar);
        assert_eq!(snap.items[&10_002].quantity, 5_000);
    });

    let install_rep2 = handle_industry_jobs(
        &ctx,
        &mut session,
        "industryManager",
        "InstallJob",
        &[
            PyRep::Integer(10_001),
            PyRep::Integer(688),
            PyRep::Integer(1),
            PyRep::Integer(2),
        ],
        &HashMap::new(),
    )
    .unwrap()
    .unwrap();
    let job_id2 = install_rep2.dict_get("jobID").unwrap().as_i64().unwrap();

    let err_not_ready = handle_industry_jobs(
        &ctx,
        &mut session,
        "industryManager",
        "CompleteJob",
        &[PyRep::Integer(job_id2)],
        &HashMap::new(),
    )
    .unwrap()
    .unwrap_err();
    assert_eq!(err_not_ready.code, "JobNotYetComplete");

    ctx.db.with_mut_snapshot(|snap| {
        snap.industry_jobs.get_mut(&job_id2).unwrap().start_unix = 0;
    });

    let complete_rep = handle_industry_jobs(
        &ctx,
        &mut session,
        "industryManager",
        "CompleteJob",
        &[PyRep::Integer(job_id2)],
        &HashMap::new(),
    )
    .unwrap()
    .unwrap();
    let delivered_id = complete_rep
        .dict_get("deliveredItemID")
        .unwrap()
        .as_i64()
        .unwrap();
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items[&10_001].flag, InventoryFlag::Hangar);
        let prod = &snap.items[&delivered_id];
        assert_eq!(prod.type_id, 587);
        assert_eq!(prod.quantity, 2);
    });
}

#[test]
fn research_me_te_copy_and_invention_jobs_execute_and_enforce_caps() {
    let mut ctx = make_test_context();
    setup_industry_sde(&mut ctx);
    let char_id = 90_000_001;
    let station_id = 60_003_760;
    let mut session = char_session(char_id, 98_000_001);
    session.station_id = Some(station_id);
    session.solar_system_id = Some(30_000_142);

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.balance = IskCents::from_f64_isk(1_000_000_000.0);
        }
        for (sid, lvl) in [(3406, 5), (11448, 5), (11452, 5), (21791, 5)] {
            snap.skills.entry(char_id).or_default().insert(
                sid,
                TrainedSkillRecord {
                    type_id: sid,
                    level: lvl,
                    skill_points: 256_000.0,
                },
            );
        }
        let mut bpo = make_item(
            20_001,
            688,
            char_id,
            station_id,
            InventoryFlag::Hangar,
            1,
            true,
        );
        bpo.dynamic_attributes.insert(ATTR_BLUEPRINT_ME, 9.0);
        bpo.dynamic_attributes.insert(ATTR_BLUEPRINT_TE, 18.0);
        snap.items.insert(20_001, bpo);
    });

    assert_eq!(
        handle_industry_jobs(
            &ctx,
            &mut session,
            "industryManager",
            "InstallJob",
            &[PyRep::Integer(688), PyRep::Integer(4), PyRep::Integer(2)],
            &HashMap::new(),
        )
        .unwrap()
        .unwrap_err()
        .code,
        "MaxMaterialEfficiencyReached"
    );
    assert_eq!(
        handle_industry_jobs(
            &ctx,
            &mut session,
            "industryManager",
            "InstallJob",
            &[PyRep::Integer(688), PyRep::Integer(3), PyRep::Integer(2)],
            &HashMap::new(),
        )
        .unwrap()
        .unwrap_err()
        .code,
        "MaxTimeEfficiencyReached"
    );
    assert_eq!(
        handle_industry_jobs(
            &ctx,
            &mut session,
            "industryManager",
            "InstallJob",
            &[PyRep::Integer(688), PyRep::Integer(5), PyRep::Integer(11)],
            &HashMap::new(),
        )
        .unwrap()
        .unwrap_err()
        .code,
        "MaxRunsPerCopyExceeded"
    );

    let me_rep = handle_industry_jobs(
        &ctx,
        &mut session,
        "industryManager",
        "InstallJob",
        &[PyRep::Integer(688), PyRep::Integer(4), PyRep::Integer(1)],
        &HashMap::new(),
    )
    .unwrap()
    .unwrap();
    let me_jid = me_rep.dict_get("jobID").unwrap().as_i64().unwrap();
    ctx.db.with_mut_snapshot(|snap| {
        snap.industry_jobs.get_mut(&me_jid).unwrap().start_unix = 0;
    });
    handle_industry_jobs(
        &ctx,
        &mut session,
        "industryManager",
        "CompleteJob",
        &[PyRep::Integer(me_jid)],
        &HashMap::new(),
    )
    .unwrap()
    .unwrap();
    ctx.db.with_snapshot(|snap| {
        assert_eq!(
            snap.items[&20_001].dynamic_attributes[&ATTR_BLUEPRINT_ME],
            10.0
        );
    });

    let copy_rep = handle_industry_jobs(
        &ctx,
        &mut session,
        "industryManager",
        "InstallJob",
        &[PyRep::Integer(688), PyRep::Integer(5), PyRep::Integer(3)],
        &HashMap::new(),
    )
    .unwrap()
    .unwrap();
    let copy_jid = copy_rep.dict_get("jobID").unwrap().as_i64().unwrap();
    ctx.db.with_mut_snapshot(|snap| {
        snap.industry_jobs.get_mut(&copy_jid).unwrap().start_unix = 0;
    });
    let copy_done = handle_industry_jobs(
        &ctx,
        &mut session,
        "industryManager",
        "CompleteManyJobs",
        &[PyRep::List(vec![PyRep::Integer(copy_jid)])],
        &HashMap::new(),
    )
    .unwrap()
    .unwrap();
    let bpc_id = match copy_done {
        PyRep::List(v) => v[0]
            .dict_get("createdBpcItemID")
            .unwrap()
            .as_i64()
            .unwrap(),
        _ => panic!("expected list"),
    };
    ctx.db.with_mut_snapshot(|snap| {
        assert!(snap.items[&bpc_id].is_blueprint_copy);
        assert_eq!(snap.items[&bpc_id].blueprint_runs_remaining, Some(3));
        snap.items.insert(
            20_050,
            make_item(
                20_050,
                20424,
                char_id,
                station_id,
                InventoryFlag::Hangar,
                10,
                false,
            ),
        );
    });

    let inv_rep = handle_industry_jobs(
        &ctx,
        &mut session,
        "industryManager",
        "InstallJob",
        &[
            PyRep::Integer(bpc_id),
            PyRep::Integer(688),
            PyRep::Integer(8),
            PyRep::Integer(1),
        ],
        &HashMap::new(),
    )
    .unwrap()
    .unwrap();
    let inv_jid = inv_rep.dict_get("jobID").unwrap().as_i64().unwrap();
    ctx.db.with_mut_snapshot(|snap| {
        snap.industry_jobs.get_mut(&inv_jid).unwrap().start_unix = 0;
    });
    let inv_done = handle_industry_jobs(
        &ctx,
        &mut session,
        "industryManager",
        "CompleteJob",
        &[PyRep::Integer(inv_jid)],
        &HashMap::new(),
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        inv_done.dict_get("inventionSucceeded").unwrap().as_bool(),
        Some(true)
    );
}
