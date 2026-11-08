use super::*;
use crate::router::ServerContext;
use eve_db::{InventoryFlag, InventoryItemRecord, IskCents, TrainedSkillRecord, WorldRepository};
use eve_proto::PyRep;
use eve_sde::{
    BlueprintActivity, BlueprintInfo, BlueprintMaterial, BlueprintProduct, InvType, SdeRegistry,
};
use std::collections::HashMap;
use std::sync::Arc;

#[cfg(test)]
#[path = "industry_research_and_invention_tests.rs"]
mod industry_research_and_invention_tests;

pub(super) fn make_test_context() -> ServerContext {
    let sde = Arc::new(SdeRegistry::with_canonical_seed());
    let db = Arc::new(WorldRepository::with_demo_seed());
    ServerContext::with_sde_and_repository(sde, db)
}

pub(super) fn char_session(char_id: i64, corp_id: i64) -> SessionState {
    SessionState {
        user_id: Some(1),
        character_id: Some(char_id),
        corporation_id: Some(corp_id),
        solar_system_id: Some(30_000_142),
        station_id: Some(60_003_760),
        ..Default::default()
    }
}

pub(super) fn make_inv_type(type_id: u32, group_id: u32, name: &str, volume: f64, base_price: f64) -> InvType {
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

pub(super) fn make_item(
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

pub(super) fn setup_industry_sde(ctx: &mut ServerContext) {
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
fn test_copy_licensed_runs_validation_scaling_and_multi_bpc_mint() {
    let mut sde = SdeRegistry::with_canonical_seed();
    sde.types.insert(688, make_inv_type(688, 105, "Rifter Blueprint", 0.01, 100_000.0));
    sde.types.insert(587, make_inv_type(587, 25, "Rifter", 27_500.0, 500_000.0));
    sde.blueprints.insert(
        688,
        BlueprintInfo {
            blueprint_type_id: 688,
            product_type_id: 587,
            product_quantity: 1,
            max_production_limit: 10,
            manufacturing_time_sec: 100,
            research_me_time_sec: 105,
            research_te_time_sec: 105,
            copy_time_sec: 100,
            invention_time_sec: 100,
            invention_base_probability: 0.30,
            materials: vec![BlueprintMaterial { type_id: 34, quantity: 100 }],
            activities: HashMap::new(),
        },
    );
    let db = Arc::new(WorldRepository::with_demo_seed());
    let ctx = ServerContext::with_sde_and_repository(Arc::new(sde), db);
    let char_id = 90_000_001;
    let corp_id = 98_000_001;
    let station_id = 60_003_760;
    let mut session = char_session(char_id, corp_id);

    let (bpo_1, bpo_2, bpo_3) = ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.balance = IskCents::from_f64_isk(1_000_000_000.0);
        }
        snap.skills.entry(char_id).or_default().insert(
            3380,
            TrainedSkillRecord { type_id: 3380, level: 5, skill_points: 256_000.0 },
        );
        snap.skills.entry(char_id).or_default().insert(
            3406,
            TrainedSkillRecord { type_id: 3406, level: 5, skill_points: 256_000.0 },
        );
        snap.items.insert(
            99_101,
            InventoryItemRecord {
                item_id: 99_101,
                type_id: 34,
                owner_id: char_id,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 10_000,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        for id in [99_201_i64, 99_202, 99_203] {
            snap.items.insert(
                id,
                InventoryItemRecord {
                    item_id: id,
                    type_id: 688,
                    owner_id: char_id,
                    location_id: station_id,
                    flag: InventoryFlag::Hangar,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
        }
        (99_201_i64, 99_202_i64, 99_203_i64)
    });

    // 1. Explicit licensedRuns rejected on non-copying activity (activityID = 1)
    let err_non_copy = handle_industry_jobs(
        &ctx,
        &mut session,
        "industryManager",
        "InstallJob",
        &[PyRep::keyval([
            ("blueprintID", PyRep::Integer(bpo_1)),
            ("activityID", PyRep::Integer(1)),
            ("runs", PyRep::Integer(1)),
            ("licensedRuns", PyRep::Integer(2)),
        ])],
        &HashMap::new(),
    )
    .unwrap()
    .unwrap_err();
    assert_eq!(err_non_copy.code, "InvalidIndustryJobParameters");

    // 2. Explicit licensedRuns > max_production_limit (11 > 10) rejected even with runs = 1
    let err_over_limit = handle_industry_jobs(
        &ctx,
        &mut session,
        "industryManager",
        "InstallJob",
        &[PyRep::keyval([
            ("blueprintID", PyRep::Integer(bpo_1)),
            ("activityID", PyRep::Integer(5)),
            ("runs", PyRep::Integer(1)),
            ("licensedRuns", PyRep::Integer(11)),
        ])],
        &HashMap::new(),
    )
    .unwrap()
    .unwrap_err();
    assert_eq!(err_over_limit.code, "MaxRunsPerCopyExceeded");

    // 3. Zero / non-positive / i64::MAX licensedRuns rejected
    let err_zero = handle_industry_jobs(
        &ctx,
        &mut session,
        "industryManager",
        "InstallJob",
        &[PyRep::keyval([
            ("blueprintID", PyRep::Integer(bpo_1)),
            ("activityID", PyRep::Integer(5)),
            ("runs", PyRep::Integer(1)),
            ("licensedRuns", PyRep::Integer(0)),
        ])],
        &HashMap::new(),
    )
    .unwrap()
    .unwrap_err();
    assert_eq!(err_zero.code, "InvalidIndustryJobParameters");

    let err_huge = handle_industry_jobs(
        &ctx,
        &mut session,
        "industryManager",
        "InstallJob",
        &[PyRep::keyval([
            ("blueprintID", PyRep::Integer(bpo_1)),
            ("activityID", PyRep::Integer(5)),
            ("runs", PyRep::Integer(1)),
            ("licensedRuns", PyRep::Integer(i64::MAX)),
        ])],
        &HashMap::new(),
    )
    .unwrap()
    .unwrap_err();
    assert_eq!(err_huge.code, "MaxRunsPerCopyExceeded");

    // 4. Positive regression: runs=1, licensedRuns=1 baseline vs runs=3, licensedRuns=5
    let base_rep = handle_industry_jobs(
        &ctx,
        &mut session,
        "industryManager",
        "InstallJob",
        &[PyRep::keyval([
            ("blueprintID", PyRep::Integer(bpo_2)),
            ("activityID", PyRep::Integer(5)),
            ("runs", PyRep::Integer(1)),
            ("licensedRuns", PyRep::Integer(1)),
        ])],
        &HashMap::new(),
    )
    .unwrap()
    .unwrap();
    let base_jid = base_rep.dict_get("jobID").unwrap().as_i64().unwrap();
    let base_dur = base_rep.dict_get("durationSeconds").unwrap().as_i64().unwrap();
    let base_cost = base_rep.dict_get("totalJobCost").unwrap().as_f64().unwrap();
    // Cancel base job so it does not skew system_cost_index (0.01 -> 0.50) for the scaled job comparison.
    handle_industry_jobs(&ctx, &mut session, "industryManager", "CancelJob", &[PyRep::Integer(base_jid)], &HashMap::new())
        .unwrap()
        .unwrap();

    let scaled_rep = handle_industry_jobs(
        &ctx,
        &mut session,
        "industryManager",
        "InstallJob",
        &[PyRep::keyval([
            ("blueprintID", PyRep::Integer(bpo_3)),
            ("activityID", PyRep::Integer(5)),
            ("runs", PyRep::Integer(3)),
            ("licensedRuns", PyRep::Integer(5)),
        ])],
        &HashMap::new(),
    )
    .unwrap()
    .unwrap();
    let scaled_jid = scaled_rep.dict_get("jobID").unwrap().as_i64().unwrap();
    let scaled_dur = scaled_rep.dict_get("durationSeconds").unwrap().as_i64().unwrap();
    let scaled_cost = scaled_rep.dict_get("totalJobCost").unwrap().as_f64().unwrap();
    assert_eq!(scaled_dur, base_dur * 15);
    assert!(
        (scaled_cost - base_cost * 15.0).abs() < 0.01,
        "Expected 15x copy cost ({} * 15 = {}), got {}",
        base_cost,
        base_cost * 15.0,
        scaled_cost
    );

    ctx.db.with_mut_snapshot(|snap| {
        snap.industry_jobs.get_mut(&scaled_jid).unwrap().start_unix = 0;
    });
    handle_industry_jobs(
        &ctx,
        &mut session,
        "industryManager",
        "CompleteJob",
        &[PyRep::Integer(scaled_jid)],
        &HashMap::new(),
    )
    .unwrap()
    .unwrap();

    ctx.db.with_snapshot(|snap| {
        let bpcs: Vec<_> = snap
            .items
            .values()
            .filter(|i| i.owner_id == char_id && i.type_id == 688 && i.is_blueprint_copy)
            .collect();
        assert_eq!(bpcs.len(), 3, "runs=3 must mint 3 distinct BPCs, got {}", bpcs.len());
        for bpc in bpcs {
            assert!(bpc.singleton);
            assert_eq!(bpc.quantity, 1);
            assert_eq!(bpc.blueprint_runs_remaining, Some(5));
        }
    });
}
