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

    let (bpo_1, bpo_2, bpo_3, bpo_mfg_lr, bpo_mfg_zero) = ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.balance = IskCents::from_f64_isk(1_000_000_000.0);
        }
        snap.skills.entry(char_id).or_default().insert(
            3380,
            TrainedSkillRecord { type_id: 3380, level: 5, skill_points: 256_000.0 },
        );
        snap.skills.entry(char_id).or_default().insert(
            3387,
            TrainedSkillRecord { type_id: 3387, level: 5, skill_points: 256_000.0 },
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
        for id in [99_201_i64, 99_202, 99_203, 99_204, 99_205] {
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
        (99_201_i64, 99_202_i64, 99_203_i64, 99_204_i64, 99_205_i64)
    });

    // 1. #964: V24 IndustryWnd always sends licensedRuns even on non-copying activities (activityID = 1);
    // it must be ignored (no error, licensed_runs stored as None, cost/duration unaffected).
    let mfg_lr_rep = handle_industry_jobs(
        &ctx,
        &mut session,
        "industryManager",
        "InstallJob",
        &[PyRep::keyval([
            ("blueprintID", PyRep::Integer(bpo_mfg_lr)),
            ("activityID", PyRep::Integer(1)),
            ("runs", PyRep::Integer(1)),
            ("licensedRuns", PyRep::Integer(2)),
        ])],
        &HashMap::new(),
    )
    .unwrap()
    .expect("non-copying InstallJob with licensedRuns=2 must succeed");
    let mfg_lr_jid = mfg_lr_rep.dict_get("jobID").unwrap().as_i64().unwrap();
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.industry_jobs[&mfg_lr_jid].licensed_runs, None);
    });
    handle_industry_jobs(
        &ctx,
        &mut session,
        "industryManager",
        "CancelJob",
        &[PyRep::Integer(mfg_lr_jid)],
        &HashMap::new(),
    )
    .unwrap()
    .unwrap();
    let mfg_zero_rep = handle_industry_jobs(
        &ctx,
        &mut session,
        "industryManager",
        "InstallJob",
        &[PyRep::keyval([
            ("blueprintID", PyRep::Integer(bpo_mfg_zero)),
            ("activityID", PyRep::Integer(1)),
            ("runs", PyRep::Integer(1)),
            ("licensedRuns", PyRep::Integer(0)),
        ])],
        &HashMap::new(),
    )
    .unwrap()
    .expect("non-copying InstallJob with licensedRuns=0 must succeed");
    let mfg_zero_jid = mfg_zero_rep.dict_get("jobID").unwrap().as_i64().unwrap();
    assert_eq!(
        mfg_lr_rep.dict_get("durationSeconds"),
        mfg_zero_rep.dict_get("durationSeconds")
    );
    assert_eq!(
        mfg_lr_rep.dict_get("totalJobCost"),
        mfg_zero_rep.dict_get("totalJobCost")
    );
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.industry_jobs[&mfg_zero_jid].licensed_runs, None);
    });
    handle_industry_jobs(
        &ctx,
        &mut session,
        "industryManager",
        "CancelJob",
        &[PyRep::Integer(mfg_zero_jid)],
        &HashMap::new(),
    )
    .unwrap()
    .unwrap();

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

#[test]
fn install_job_v24_ignores_corporation_id_and_enforces_account_rbac() {
    let mut sde = SdeRegistry::with_canonical_seed();
    sde.types.insert(688, make_inv_type(688, 105, "Rifter Blueprint", 0.01, 100_000.0));
    sde.types.insert(587, make_inv_type(587, 25, "Rifter", 27_500.0, 500_000.0));
    sde.blueprints.insert(
        688,
        BlueprintInfo {
            blueprint_type_id: 688,
            product_type_id: 587,
            product_quantity: 1,
            max_production_limit: 30,
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
    let char_id = 90_000_001_i64;
    let other_char = 90_000_002_i64;
    let corp_id = 98_100_001_i64;
    let station_id = 60_003_760_i64;
    let mut session = char_session(char_id, corp_id);
    session.corp_role = 0;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.corporation_id = corp_id;
            ch.balance = IskCents::from_isk(1_000_000_000);
        }
        let mut corp = eve_db::CorporationRecord::new(corp_id, "V24 Test Corp", "V24T", other_char);
        corp.set_division_balance(1, IskCents(0));
        corp.set_division_balance(2, IskCents::from_isk(500_000_000));
        snap.corporations.insert(corp_id, corp);

        for (tid, lvl) in [(3380, 5), (3387, 5), (3406, 5)] {
            snap.skills.entry(char_id).or_default().insert(
                tid,
                TrainedSkillRecord { type_id: tid, level: lvl, skill_points: 256_000.0 },
            );
        }
        snap.items.insert(
            99_500,
            InventoryItemRecord {
                item_id: 99_500,
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
        for bp_id in [99_501_i64, 99_502, 99_503] {
            snap.items.insert(
                bp_id,
                InventoryItemRecord {
                    item_id: bp_id,
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
        snap.items.insert(
            99_504,
            InventoryItemRecord {
                item_id: 99_504,
                type_id: 688,
                owner_id: corp_id,
                location_id: station_id,
                flag: InventoryFlag::CorpSAG1,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            99_505,
            InventoryItemRecord {
                item_id: 99_505,
                type_id: 34,
                owner_id: corp_id,
                location_id: station_id,
                flag: InventoryFlag::CorpSAG1,
                quantity: 1_000,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });

    // 1. Personal V24.01 payload: unconditionally sends corporationID, licensedRuns, and account=(char_id, 1000).
    // Even for a zero-role corp member whose corp Division 1 has 0 ISK, manufacturing and copying succeed as personal jobs.
    let wallet_field = "account";
    let bal_before = ctx.db.with_snapshot(|s| s.characters[&char_id].balance);
    let mfg_rep = handle_industry_jobs(
        &ctx,
        &mut session,
        "industryManager",
        "InstallJob",
        &[PyRep::keyval([
            ("blueprintID", PyRep::Integer(99_501)),
            ("blueprintTypeID", PyRep::Integer(688)),
            ("activityID", PyRep::Integer(1)),
            ("runs", PyRep::Integer(1)),
            ("licensedRuns", PyRep::Integer(1)),
            ("corporationID", PyRep::Integer(corp_id)),
            (
                wallet_field,
                PyRep::Tuple(vec![PyRep::Integer(char_id), PyRep::Integer(1000)]),
            ),
        ])],
        &HashMap::new(),
    )
    .unwrap()
    .expect("V24 personal manufacturing job must succeed");
    assert_eq!(mfg_rep.dict_get("ownerID"), Some(&PyRep::Integer(char_id)));
    let mfg_jid = mfg_rep.dict_get("jobID").unwrap().as_i64().unwrap();

    let copy_rep = handle_industry_jobs(
        &ctx,
        &mut session,
        "industryManager",
        "InstallJob",
        &[PyRep::keyval([
            ("blueprintID", PyRep::Integer(99_502)),
            ("blueprintTypeID", PyRep::Integer(688)),
            ("activityID", PyRep::Integer(5)),
            ("runs", PyRep::Integer(1)),
            ("licensedRuns", PyRep::Integer(30)),
            ("corporationID", PyRep::Integer(corp_id)),
            (
                wallet_field,
                PyRep::Tuple(vec![PyRep::Integer(char_id), PyRep::Integer(1000)]),
            ),
        ])],
        &HashMap::new(),
    )
    .unwrap()
    .expect("V24 personal copying job must succeed");
    assert_eq!(copy_rep.dict_get("ownerID"), Some(&PyRep::Integer(char_id)));
    let copy_jid = copy_rep.dict_get("jobID").unwrap().as_i64().unwrap();
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.industry_jobs[&mfg_jid].licensed_runs, None);
        assert_eq!(snap.industry_jobs[&copy_jid].licensed_runs, Some(30));
        assert!(snap.characters[&char_id].balance < bal_before);
        assert_eq!(snap.corporations[&corp_id].division_balance(1), IskCents(0));
    });

    // 2. Foreign ownerID and foreign / out-of-range wallet accounts are rejected.
    let err_foreign_owner = handle_industry_jobs(
        &ctx,
        &mut session,
        "industryManager",
        "InstallJob",
        &[PyRep::keyval([
            ("blueprintID", PyRep::Integer(99_503)),
            ("blueprintTypeID", PyRep::Integer(688)),
            ("activityID", PyRep::Integer(1)),
            ("runs", PyRep::Integer(1)),
            ("ownerID", PyRep::Integer(other_char)),
        ])],
        &HashMap::new(),
    )
    .unwrap()
    .unwrap_err();
    assert_eq!(err_foreign_owner.code, "OwnerAccessDenied");

    let err_foreign_wallet = handle_industry_jobs(
        &ctx,
        &mut session,
        "industryManager",
        "InstallJob",
        &[PyRep::keyval([
            ("blueprintID", PyRep::Integer(99_503)),
            ("blueprintTypeID", PyRep::Integer(688)),
            ("activityID", PyRep::Integer(1)),
            ("runs", PyRep::Integer(1)),
            (
                wallet_field,
                PyRep::Tuple(vec![PyRep::Integer(other_char), PyRep::Integer(1000)]),
            ),
        ])],
        &HashMap::new(),
    )
    .unwrap()
    .unwrap_err();
    assert_eq!(err_foreign_wallet.code, "OwnerAccessDenied");

    let err_bad_div = handle_industry_jobs(
        &ctx,
        &mut session,
        "industryManager",
        "InstallJob",
        &[PyRep::keyval([
            ("blueprintID", PyRep::Integer(99_503)),
            ("blueprintTypeID", PyRep::Integer(688)),
            ("activityID", PyRep::Integer(1)),
            ("runs", PyRep::Integer(1)),
            (
                wallet_field,
                PyRep::Tuple(vec![PyRep::Integer(corp_id), PyRep::Integer(1007)]),
            ),
        ])],
        &HashMap::new(),
    )
    .unwrap()
    .unwrap_err();
    assert_eq!(err_bad_div.code, "InvalidIndustryJobParameters");

    // 3. Corp wallet account=(corp_id, 1001) requires Division 2 Account Take role; once granted, debits Division 2.
    let err_corp_wallet_no_role = handle_industry_jobs(
        &ctx,
        &mut session,
        "industryManager",
        "InstallJob",
        &[PyRep::keyval([
            ("blueprintID", PyRep::Integer(99_503)),
            ("blueprintTypeID", PyRep::Integer(688)),
            ("activityID", PyRep::Integer(1)),
            ("runs", PyRep::Integer(1)),
            (
                wallet_field,
                PyRep::Tuple(vec![PyRep::Integer(corp_id), PyRep::Integer(1001)]),
            ),
        ])],
        &HashMap::new(),
    )
    .unwrap()
    .unwrap_err();
    assert_eq!(err_corp_wallet_no_role.code, "CrpAccessDenied");

    let acc2_role = eve_db::corp_roles::ROLE_ACCOUNT_CAN_TAKE[1];
    session.corp_role = acc2_role;
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations
            .get_mut(&corp_id)
            .unwrap()
            .member_roles
            .insert(char_id, acc2_role);
    });
    // Without Factory Manager, corp blueprint install still fails even with Account Take 2.
    let err_corp_bp_no_factory = handle_industry_jobs(
        &ctx,
        &mut session,
        "industryManager",
        "InstallJob",
        &[PyRep::keyval([
            ("blueprintID", PyRep::Integer(99_504)),
            ("blueprintTypeID", PyRep::Integer(688)),
            ("activityID", PyRep::Integer(1)),
            ("runs", PyRep::Integer(1)),
            ("corporationID", PyRep::Integer(corp_id)),
            (
                wallet_field,
                PyRep::Tuple(vec![PyRep::Integer(corp_id), PyRep::Integer(1001)]),
            ),
        ])],
        &HashMap::new(),
    )
    .unwrap()
    .unwrap_err();
    assert_eq!(err_corp_bp_no_factory.code, "CrpAccessDenied");

    let div2_before = ctx
        .db
        .with_snapshot(|s| s.corporations[&corp_id].division_balance(2));
    let char_bal_before_corp_pay = ctx.db.with_snapshot(|s| s.characters[&char_id].balance);
    let pers_corp_pay = handle_industry_jobs(
        &ctx,
        &mut session,
        "industryManager",
        "InstallJob",
        &[PyRep::keyval([
            ("blueprintID", PyRep::Integer(99_503)),
            ("blueprintTypeID", PyRep::Integer(688)),
            ("activityID", PyRep::Integer(1)),
            ("runs", PyRep::Integer(1)),
            (
                wallet_field,
                PyRep::Tuple(vec![PyRep::Integer(corp_id), PyRep::Integer(1001)]),
            ),
        ])],
        &HashMap::new(),
    )
    .unwrap()
    .expect("personal job funded by authorized corp division 2 wallet must succeed");
    assert_eq!(pers_corp_pay.dict_get("ownerID"), Some(&PyRep::Integer(char_id)));
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.characters[&char_id].balance, char_bal_before_corp_pay);
        assert!(snap.corporations[&corp_id].division_balance(2) < div2_before);
        assert_eq!(snap.corporations[&corp_id].division_balance(1), IskCents(0));
        assert!(!snap
            .corp_wallet_journals
            .get(&(corp_id, 2))
            .map_or(true, |v| v.is_empty()));
    });

    // 4. Granting Factory Manager + Div 1 Hangar Take allows V24 corp blueprint install using corp Div 2 wallet.
    let full_roles = acc2_role
        | eve_db::corp_roles::ROLE_FACTORY_MANAGER
        | eve_db::corp_roles::ROLE_HANGAR_CAN_TAKE[0];
    session.corp_role = full_roles;
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations
            .get_mut(&corp_id)
            .unwrap()
            .member_roles
            .insert(char_id, full_roles);
    });
    let corp_rep = handle_industry_jobs(
        &ctx,
        &mut session,
        "industryManager",
        "InstallJob",
        &[PyRep::keyval([
            ("blueprintID", PyRep::Integer(99_504)),
            ("blueprintTypeID", PyRep::Integer(688)),
            ("activityID", PyRep::Integer(1)),
            ("runs", PyRep::Integer(1)),
            ("licensedRuns", PyRep::Integer(1)),
            ("useCorp", PyRep::Bool(true)),
            ("corporationID", PyRep::Integer(corp_id)),
            (
                wallet_field,
                PyRep::Tuple(vec![PyRep::Integer(corp_id), PyRep::Integer(1001)]),
            ),
        ])],
        &HashMap::new(),
    )
    .unwrap()
    .expect("authorized corp job with V24 payload must succeed");
    assert_eq!(corp_rep.dict_get("ownerID"), Some(&PyRep::Integer(corp_id)));
}

#[test]
fn test_p0_1176_u64_material_overflow_and_multistack_refund_delivery() {
    let mut sde = SdeRegistry::with_canonical_seed();
    // Synthetic extreme-output & overflow blueprints for bounds validation
    sde.blueprints.insert(
        999_101,
        BlueprintInfo {
            blueprint_type_id: 999_101,
            product_type_id: 587,
            product_quantity: 1,
            max_production_limit: 10,
            manufacturing_time_sec: 0,
            research_me_time_sec: 100,
            research_te_time_sec: 100,
            copy_time_sec: 100,
            invention_time_sec: 100,
            invention_base_probability: 0.0,
            materials: vec![BlueprintMaterial { type_id: 34, quantity: u64::MAX }],
            activities: HashMap::new(),
        },
    );
    sde.blueprints.insert(
        999_102,
        BlueprintInfo {
            blueprint_type_id: 999_102,
            product_type_id: 587,
            product_quantity: 1_500_000_000,
            max_production_limit: 10,
            manufacturing_time_sec: 600,
            research_me_time_sec: 100,
            research_te_time_sec: 100,
            copy_time_sec: 100,
            invention_time_sec: 100,
            invention_base_probability: 0.0,
            materials: vec![BlueprintMaterial { type_id: 34, quantity: 10 }],
            activities: HashMap::from([(
                "manufacturing".into(),
                BlueprintActivity {
                    time: 600,
                    materials: vec![BlueprintMaterial { type_id: 34, quantity: 10 }],
                    products: vec![BlueprintProduct {
                        type_id: 587,
                        quantity: 1_500_000_000,
                        probability: None,
                    }],
                    skills: vec![],
                },
            )]),
        },
    );
    sde.blueprints.insert(
        999_103,
        BlueprintInfo {
            blueprint_type_id: 999_103,
            product_type_id: 16_670,
            product_quantity: 1,
            max_production_limit: 100,
            manufacturing_time_sec: 0,
            research_me_time_sec: 0,
            research_te_time_sec: 0,
            copy_time_sec: 0,
            invention_time_sec: 0,
            invention_base_probability: 0.0,
            materials: vec![],
            activities: HashMap::from([(
                "reaction".into(),
                BlueprintActivity {
                    time: 3_600,
                    materials: vec![BlueprintMaterial { type_id: 34, quantity: 1_500_000_000 }],
                    products: vec![BlueprintProduct { type_id: 16_670, quantity: 1, probability: None }],
                    skills: vec![],
                },
            )]),
        },
    );
    sde.blueprints.insert(
        999_104,
        BlueprintInfo {
            blueprint_type_id: 999_104,
            product_type_id: 587,
            product_quantity: 1,
            max_production_limit: 1,
            manufacturing_time_sec: 9_000_000,
            research_me_time_sec: 100,
            research_te_time_sec: 100,
            copy_time_sec: 100,
            invention_time_sec: 100,
            invention_base_probability: 0.0,
            materials: vec![BlueprintMaterial { type_id: 34, quantity: 10 }],
            activities: HashMap::new(),
        },
    );
    let ctx = ServerContext::with_sde_and_repository(
        Arc::new(sde),
        Arc::new(WorldRepository::with_demo_seed()),
    );
    let char_id = 90_000_001_i64;
    let corp_id = ctx.db.with_snapshot(|s| s.characters[&char_id].corporation_id);
    let station_id = 60_003_760_i64;
    let mut sess = char_session(char_id, corp_id);
    let kw = HashMap::new();

    // Seed BPO 37030 (80M Tritanium/run, 72_000s/run) with secondary minerals only (0 Tritanium).
    let bp_37030_id = 990_001_i64;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.balance = IskCents::from_isk(100_000_000_000);
        }
        snap.skills.entry(char_id).or_default().extend([
            (3380, TrainedSkillRecord { type_id: 3380, level: 5, skill_points: 256_000.0 }),
            (3387, TrainedSkillRecord { type_id: 3387, level: 5, skill_points: 256_000.0 }),
            (3388, TrainedSkillRecord { type_id: 3388, level: 5, skill_points: 256_000.0 }),
            (3400, TrainedSkillRecord { type_id: 3400, level: 5, skill_points: 256_000.0 }),
            (24625, TrainedSkillRecord { type_id: 24625, level: 5, skill_points: 256_000.0 }),
            (45746, TrainedSkillRecord { type_id: 45746, level: 5, skill_points: 256_000.0 }),
        ]);
        // Remove starter Tritanium so hangar has 0 Tritanium.
        snap.items.retain(|_, it| !(it.owner_id == char_id && it.type_id == 34));
        let mut insert_item = |id: i64, tid: u32, loc: i64, qty: i32, single: bool| {
            snap.items.insert(
                id,
                InventoryItemRecord {
                    item_id: id,
                    type_id: tid,
                    owner_id: char_id,
                    location_id: loc,
                    flag: InventoryFlag::Hangar,
                    quantity: qty,
                    singleton: single,
                    version: 1,
                    ..Default::default()
                },
            );
        };
        insert_item(bp_37030_id, 37_030, station_id, 1, true);
        let bp_info = ctx.sde.blueprints[&37_030].clone();
        for (idx, m) in bp_info.materials.iter().enumerate() {
            if m.type_id != 34 {
                insert_item(990_010 + idx as i64, m.type_id, station_id, (m.quantity * 27) as i32, false);
            }
        }
    });

    let jobs_before = ctx.db.with_snapshot(|s| s.industry_jobs.len());
    let items_before = ctx.db.with_snapshot(|s| s.items.len());

    // 1. 27 runs on 37030 requires 2,160,000,000 Tritanium (> i32::MAX); with 0 Tritanium,
    // must fail cleanly with InsufficientManufacturingMaterials (never wrap negative and install!).
    let err_27 = handle_industry_jobs(
        &ctx,
        &mut sess,
        "industryManager",
        "InstallJob",
        &[PyRep::keyval([
            ("blueprintID", PyRep::Integer(bp_37030_id)),
            ("blueprintTypeID", PyRep::Integer(37_030)),
            ("facilityID", PyRep::Integer(station_id)),
            ("activityID", PyRep::Integer(1)),
            ("runs", PyRep::Integer(27)),
        ])],
        &kw,
    )
    .unwrap()
    .unwrap_err();
    assert_eq!(err_27.code, "InsufficientManufacturingMaterials");

    // 2. 135 runs on 37030 (9,720,000s > 30 days) fails cleanly on run/duration limit before mutation.
    let err_135 = handle_industry_jobs(
        &ctx,
        &mut sess,
        "industryManager",
        "InstallJob",
        &[PyRep::keyval([
            ("blueprintID", PyRep::Integer(bp_37030_id)),
            ("blueprintTypeID", PyRep::Integer(37_030)),
            ("facilityID", PyRep::Integer(station_id)),
            ("activityID", PyRep::Integer(1)),
            ("runs", PyRep::Integer(135)),
        ])],
        &kw,
    )
    .unwrap()
    .unwrap_err();
    assert_eq!(err_135.code, "InvalidIndustryJobParameters");
    ctx.db.with_snapshot(|s| {
        assert_eq!(s.industry_jobs.len(), jobs_before);
        assert_eq!(s.items.len(), items_before);
        assert_eq!(s.items[&bp_37030_id].flag, InventoryFlag::Hangar);
    });

    // 3. With 2,500,000,000 Tritanium split across two stacks, 27 runs installs, consumes exact
    // 2,160,000,000 (leaving 340,000,000), records i64 metadata, and CancelJob refunds full 2.16B!
    ctx.db.with_mut_snapshot(|snap| {
        for (id, qty) in [(990_050_i64, 1_500_000_000_i32), (990_051_i64, 1_000_000_000_i32)] {
            snap.items.insert(
                id,
                InventoryItemRecord {
                    item_id: id,
                    type_id: 34,
                    owner_id: char_id,
                    location_id: station_id,
                    flag: InventoryFlag::Hangar,
                    quantity: qty,
                    singleton: false,
                    version: 1,
                    ..Default::default()
                },
            );
        }
    });
    let ok_27 = handle_industry_jobs(
        &ctx,
        &mut sess,
        "industryManager",
        "InstallJob",
        &[PyRep::keyval([
            ("blueprintID", PyRep::Integer(bp_37030_id)),
            ("blueprintTypeID", PyRep::Integer(37_030)),
            ("facilityID", PyRep::Integer(station_id)),
            ("activityID", PyRep::Integer(1)),
            ("runs", PyRep::Integer(27)),
        ])],
        &kw,
    )
    .unwrap()
    .expect("27 runs with 2.5B Tritanium across stacks must succeed");
    let jid_27 = ok_27.dict_get("jobID").and_then(|v| v.as_i64()).unwrap();
    ctx.db.with_snapshot(|s| {
        let trit_left: i64 = s
            .items
            .values()
            .filter(|i| i.owner_id == char_id && i.location_id == station_id && i.type_id == 34)
            .map(|i| i64::from(i.quantity))
            .sum();
        assert_eq!(trit_left, 340_000_000);
        let meta = crate::services::industry_pi::load_job_cost_meta(s, jid_27).unwrap();
        assert!(meta.consumed_materials.contains(&(34, 2_160_000_000)));
    });
    handle_industry_jobs(
        &ctx,
        &mut sess,
        "industryManager",
        "CancelJob",
        &[PyRep::Integer(jid_27)],
        &kw,
    )
    .unwrap()
    .expect("CancelJob must refund full 2.16B across valid i32 stacks");
    ctx.db.with_snapshot(|s| {
        let trit_restored: i64 = s
            .items
            .values()
            .filter(|i| {
                i.owner_id == char_id && i.location_id == station_id && i.type_id == 34 && i.quantity > 0
            })
            .map(|i| i64::from(i.quantity))
            .sum();
        assert_eq!(trit_restored, 2_500_000_000);
        assert_eq!(s.items[&bp_37030_id].flag, InventoryFlag::Hangar);
    });

    // 4. Overflow u64::MAX material requirement rejects cleanly before mutation; 1-run >30d capital-time
    // blueprint is allowed; huge product output (3B) delivers across valid i32 stacks without wrap.
    ctx.db.with_mut_snapshot(|snap| {
        for (id, tid) in [(990_060_i64, 999_101_u32), (990_061_i64, 999_102_u32), (990_062_i64, 999_104_u32)] {
            snap.items.insert(
                id,
                InventoryItemRecord {
                    item_id: id,
                    type_id: tid,
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
    });
    let err_u64 = handle_industry_jobs(
        &ctx,
        &mut sess,
        "industryManager",
        "InstallJob",
        &[PyRep::keyval([
            ("blueprintID", PyRep::Integer(990_060)),
            ("blueprintTypeID", PyRep::Integer(999_101)),
            ("facilityID", PyRep::Integer(station_id)),
            ("activityID", PyRep::Integer(1)),
            ("runs", PyRep::Integer(2)),
        ])],
        &kw,
    )
    .unwrap()
    .unwrap_err();
    assert_eq!(err_u64.code, "InvalidIndustryJobParameters");

    let ok_1run_long = handle_industry_jobs(
        &ctx,
        &mut sess,
        "industryManager",
        "InstallJob",
        &[PyRep::keyval([
            ("blueprintID", PyRep::Integer(990_062)),
            ("blueprintTypeID", PyRep::Integer(999_104)),
            ("facilityID", PyRep::Integer(station_id)),
            ("activityID", PyRep::Integer(1)),
            ("runs", PyRep::Integer(1)),
        ])],
        &kw,
    )
    .unwrap()
    .expect("1-run manufacturing >30d base time must remain permitted");
    let jid_long = ok_1run_long.dict_get("jobID").and_then(|v| v.as_i64()).unwrap();
    handle_industry_jobs(
        &ctx,
        &mut sess,
        "industryManager",
        "CancelJob",
        &[PyRep::Integer(jid_long)],
        &kw,
    )
    .unwrap()
    .expect("cancel long single-run job");

    let ok_big_out = handle_industry_jobs(
        &ctx,
        &mut sess,
        "industryManager",
        "InstallJob",
        &[PyRep::keyval([
            ("blueprintID", PyRep::Integer(990_061)),
            ("blueprintTypeID", PyRep::Integer(999_102)),
            ("facilityID", PyRep::Integer(station_id)),
            ("activityID", PyRep::Integer(1)),
            ("runs", PyRep::Integer(2)),
        ])],
        &kw,
    )
    .unwrap()
    .expect("2 runs with 1.5B output/run must install");
    let jid_out = ok_big_out.dict_get("jobID").and_then(|v| v.as_i64()).unwrap();
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.retain(|_, i| !(i.owner_id == char_id && i.type_id == 587 && !i.singleton));
        snap.industry_jobs.get_mut(&jid_out).unwrap().start_unix = 0;
    });
    handle_industry_jobs(
        &ctx,
        &mut sess,
        "industryManager",
        "CompleteJob",
        &[PyRep::Integer(jid_out)],
        &kw,
    )
    .unwrap()
    .expect("CompleteJob 3B output across valid i32 stacks");
    ctx.db.with_snapshot(|snap| {
        let stacks: Vec<i32> = snap
            .items
            .values()
            .filter(|i| i.owner_id == char_id && i.location_id == station_id && i.type_id == 587 && !i.singleton)
            .map(|i| i.quantity)
            .collect();
        assert_eq!(stacks.len(), 2);
        assert!(stacks.iter().all(|&q| q > 0));
        let delivered_sum: i64 = stacks.iter().map(|&q| i64::from(q)).sum();
        assert_eq!(delivered_sum, 3_000_000_000);
    });

    // 5. Reaction (activity 9) requiring 3,000,000,000 (> i32::MAX) with 0 stock rejects cleanly.
    let lowsec_sys = ctx
        .sde
        .solar_systems
        .values()
        .filter(|s| s.solar_system_id < 30_100_000 && s.security_status > 0.05 && s.security_status < 0.40)
        .min_by_key(|s| s.solar_system_id)
        .unwrap()
        .solar_system_id;
    let ls_athanor = 1_099_999_001_i64;
    let react_bp_id = 990_070_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            ls_athanor,
            eve_db::UpwellStructureRecord {
                structure_id: ls_athanor,
                type_id: 35_835,
                owner_corp_id: corp_id,
                solar_system_id: lowsec_sys,
                name: "Test Athanor".into(),
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 500,
                ..Default::default()
            },
        );
        snap.items.insert(
            990_071,
            InventoryItemRecord {
                item_id: 990_071,
                type_id: 45_537,
                owner_id: corp_id,
                location_id: ls_athanor,
                flag: InventoryFlag::ServiceSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.module_online_states.insert(990_071, true);
        snap.items.insert(
            react_bp_id,
            InventoryItemRecord {
                item_id: react_bp_id,
                type_id: 999_103,
                owner_id: char_id,
                location_id: ls_athanor,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.retain(|_, i| !(i.owner_id == char_id && i.type_id == 34));
    });
    sess.station_id = None;
    sess.structure_id = Some(ls_athanor);
    sess.solar_system_id = Some(lowsec_sys);
    let react_err = handle_industry_jobs(
        &ctx,
        &mut sess,
        "industryManager",
        "InstallJob",
        &[PyRep::keyval([
            ("blueprintID", PyRep::Integer(react_bp_id)),
            ("blueprintTypeID", PyRep::Integer(999_103)),
            ("facilityID", PyRep::Integer(ls_athanor)),
            ("activityID", PyRep::Integer(9)),
            ("runs", PyRep::Integer(2)),
        ])],
        &kw,
    )
    .unwrap()
    .unwrap_err();
    assert_eq!(react_err.code, "InsufficientReactionMaterials");
}

fn change_dict_contains(dict: &PyRep, key: i64, expected_val: i64) -> bool {
    matches!(
        dict,
        PyRep::Dict(entries)
            if entries
                .iter()
                .any(|(k, v)| k == &PyRep::Integer(key) && v == &PyRep::Integer(expected_val))
    )
}

fn change_dict_has_key(dict: &PyRep, key: i64) -> bool {
    matches!(
        dict,
        PyRep::Dict(entries) if entries.iter().any(|(k, _)| k == &PyRep::Integer(key))
    )
}

fn make_bp_lock_test_context() -> (ServerContext, i64, i64, i64, i64) {
    let mut sde = SdeRegistry::with_canonical_seed();
    let mut bp_type = make_inv_type(688, 105, "Rifter Blueprint", 0.01, 100_000.0);
    bp_type.category_id = 9;
    sde.types.insert(688, bp_type);
    sde.types
        .insert(587, make_inv_type(587, 25, "Rifter", 27_500.0, 500_000.0));
    let mut cont_type = make_inv_type(3293, 448, "Station Container", 100.0, 1_000_000.0);
    cont_type.category_id = 2;
    sde.types.insert(3293, cont_type);
    sde.blueprints.insert(
        688,
        BlueprintInfo {
            blueprint_type_id: 688,
            product_type_id: 587,
            product_quantity: 1,
            max_production_limit: 30,
            manufacturing_time_sec: 100,
            research_me_time_sec: 105,
            research_te_time_sec: 105,
            copy_time_sec: 100,
            invention_time_sec: 100,
            invention_base_probability: 0.30,
            materials: vec![BlueprintMaterial {
                type_id: 34,
                quantity: 100,
            }],
            activities: HashMap::new(),
        },
    );
    let db = Arc::new(WorldRepository::with_demo_seed());
    let ctx = ServerContext::with_sde_and_repository(Arc::new(sde), db);
    crate::router::delta_item_compare::set_item_compare_override_for_test(&ctx, Some(false), Some(None));
    let char_id = 90_000_001_i64;
    let peer_char_id = 90_000_002_i64;
    let corp_id = 98_160_201_i64;
    let station_id = 60_003_760_i64;
    let (u1, u2) = ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.corporation_id = corp_id;
            ch.station_id = Some(station_id);
            ch.structure_id = None;
            ch.solar_system_id = 30_000_142;
            ch.balance = IskCents::from_isk(1_000_000_000);
        }
        if let Some(ch) = snap.characters.get_mut(&peer_char_id) {
            ch.corporation_id = corp_id;
            ch.station_id = Some(station_id);
            ch.structure_id = None;
            ch.solar_system_id = 30_000_142;
        }
        let mut corp =
            eve_db::CorporationRecord::new(corp_id, "Lock Notify Corp", "LNC", char_id);
        corp.members.insert(char_id);
        corp.members.insert(peer_char_id);
        corp.set_division_balance(1, IskCents::from_isk(500_000_000));
        snap.corporations.insert(corp_id, corp);
        for cid in [char_id, peer_char_id] {
            for tid in [3380_u32, 3387, 3402, 3406, 3409] {
                snap.skills.entry(cid).or_default().insert(
                    tid,
                    TrainedSkillRecord {
                        type_id: tid,
                        level: 5,
                        skill_points: 256_000.0,
                    },
                );
            }
        }
        (
            snap.characters[&char_id].user_id,
            snap.characters[&peer_char_id].user_id,
        )
    });
    let _ = ctx
        .db
        .acquire_online_lock(u1, 1602, Some(char_id), true, ctx.now_unix())
        .expect("online lock char_id");
    let _ = ctx
        .db
        .acquire_online_lock(u2, 1603, Some(peer_char_id), true, ctx.now_unix())
        .expect("online lock peer");
    (ctx, char_id, peer_char_id, corp_id, station_id)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn install_job_emits_on_item_change_for_locked_bp_with_switches_off_and_on() {
    use crate::router::delta_item_compare::{
        flush_item_compare_summary, set_item_compare_override_for_test,
    };
    use crate::router::ProjectedItemSwitchConfig;
    use eve_testkit::shadow_client::ShadowClient;

    for switch_on in [false, true] {
        let (ctx, char_id, peer_char_id, corp_id, station_id) = make_bp_lock_test_context();
        let bpo_id = 995_101_i64 + if switch_on { 100 } else { 0 };
        let bpc_id = bpo_id + 1;
        let corp_bpo_id = bpo_id + 2;
        let corp_cont_id = bpo_id + 4;
        let corp_cont_bpo_id = bpo_id + 5;
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(
                bpo_id,
                InventoryItemRecord {
                    item_id: bpo_id,
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
            snap.items.insert(
                bpc_id,
                InventoryItemRecord {
                    item_id: bpc_id,
                    type_id: 688,
                    owner_id: char_id,
                    location_id: station_id,
                    flag: InventoryFlag::Hangar,
                    quantity: 1,
                    singleton: true,
                    is_blueprint_copy: true,
                    blueprint_runs_remaining: Some(3),
                    version: 1,
                    ..Default::default()
                },
            );
            snap.items.insert(
                corp_bpo_id,
                InventoryItemRecord {
                    item_id: corp_bpo_id,
                    type_id: 688,
                    owner_id: corp_id,
                    location_id: station_id,
                    flag: InventoryFlag::CorpSAG2,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
            snap.items.insert(
                bpo_id + 3,
                InventoryItemRecord {
                    item_id: bpo_id + 3,
                    type_id: 34,
                    owner_id: char_id,
                    location_id: station_id,
                    flag: InventoryFlag::Hangar,
                    quantity: 500,
                    singleton: false,
                    version: 1,
                    ..Default::default()
                },
            );
            snap.items.insert(
                corp_cont_id,
                InventoryItemRecord {
                    item_id: corp_cont_id,
                    type_id: 3293,
                    owner_id: corp_id,
                    location_id: station_id,
                    flag: InventoryFlag::CorpSAG2,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
            snap.items.insert(
                corp_cont_bpo_id,
                InventoryItemRecord {
                    item_id: corp_cont_bpo_id,
                    type_id: 688,
                    owner_id: corp_id,
                    location_id: corp_cont_id,
                    flag: InventoryFlag::Unlocked,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
        });

        ctx.set_projected_item_switches(ProjectedItemSwitchConfig {
            char_same_scene_item_mutation: switch_on,
            char_same_scene_item_deleted: switch_on,
            char_same_scene_item_outside_rpc: false,
            char_same_scene_item_fitted: false,
            char_all_items: false,
        });
        set_item_compare_override_for_test(&ctx, Some(true), Some(None));

        let mut sess = char_session(char_id, corp_id);
        sess.session_id = 1602;
        sess.user_id = Some(ctx.db.with_snapshot(|s| s.characters[&char_id].user_id));
        let kw = HashMap::new();
        let _ = ctx.drain_notifications_for_char(char_id);
        let _ = ctx.drain_notifications_for_char(peer_char_id);

        let mut shadow = ShadowClient::from_world(char_id, &ctx.db, Some(&ctx.sde));
        shadow.assert_converged_with_world(&ctx.db, Some(&ctx.sde));

        // 1. Personal singleton BPO Copy job (0 materials) -> emits Hangar (4) -> Locked (63)
        ctx.dispatch_rpc(
            &mut sess,
            "industryManager",
            None,
            "InstallJob",
            &[PyRep::keyval([
                ("blueprintID", PyRep::Integer(bpo_id)),
                ("blueprintTypeID", PyRep::Integer(688)),
                ("facilityID", PyRep::Integer(station_id)),
                ("activityID", PyRep::Integer(5)),
                ("runs", PyRep::Integer(1)),
                ("licensedRuns", PyRep::Integer(1)),
            ])],
            &kw,
            None,
        )
        .await
        .expect("personal BPO Copy InstallJob");

        let bpo_notifs = ctx.drain_notifications_for_char(char_id);
        let bpo_changes: Vec<_> = bpo_notifs
            .iter()
            .filter(|n| {
                n.event == "OnItemChange"
                    && n.args.first().and_then(|r| r.dbrow_get("itemID")).and_then(|v| v.as_i64())
                        == Some(bpo_id)
            })
            .collect();
        assert_eq!(
            bpo_changes.len(),
            1,
            "switch_on={switch_on}: expected single OnItemChange for locked BPO"
        );
        assert_eq!(
            bpo_changes[0].args[0].dbrow_get("flagID"),
            Some(&PyRep::Integer(InventoryFlag::Locked as i64))
        );
        assert!(change_dict_contains(
            &bpo_changes[0].args[1],
            4,
            InventoryFlag::Hangar as i64
        ));
        assert!(!change_dict_has_key(&bpo_changes[0].args[1], 5));
        let bpo_bp_updated: Vec<_> = bpo_notifs
            .iter()
            .filter(|n| n.event == "OnBlueprintsUpdated")
            .collect();
        assert_eq!(
            bpo_bp_updated.len(),
            1,
            "switch_on={switch_on}: personal InstallJob must emit OnBlueprintsUpdated once"
        );
        assert_eq!(bpo_bp_updated[0].id_type, "ownerID");
        assert_eq!(bpo_bp_updated[0].args, vec![PyRep::Integer(char_id)]);
        assert!(!bpo_notifs.iter().any(|n| n.event == "OnMultipleItemChange"));

        for n in &bpo_notifs {
            shadow.apply_notification(&n.event, &n.args);
        }
        shadow.assert_converged_with_world(&ctx.db, Some(&ctx.sde));

        // 2. Personal singleton BPC Manufacturing job -> emits Hangar (4) -> Locked (63)
        ctx.dispatch_rpc(
            &mut sess,
            "industryManager",
            None,
            "InstallJob",
            &[PyRep::keyval([
                ("blueprintID", PyRep::Integer(bpc_id)),
                ("blueprintTypeID", PyRep::Integer(688)),
                ("facilityID", PyRep::Integer(station_id)),
                ("activityID", PyRep::Integer(1)),
                ("runs", PyRep::Integer(1)),
            ])],
            &kw,
            None,
        )
        .await
        .expect("personal BPC Mfg InstallJob");

        let bpc_notifs = ctx.drain_notifications_for_char(char_id);
        let bpc_changes: Vec<_> = bpc_notifs
            .iter()
            .filter(|n| {
                n.event == "OnItemChange"
                    && n.args.first().and_then(|r| r.dbrow_get("itemID")).and_then(|v| v.as_i64())
                        == Some(bpc_id)
            })
            .collect();
        assert_eq!(
            bpc_changes.len(),
            1,
            "switch_on={switch_on}: expected single OnItemChange for locked BPC"
        );
        assert_eq!(
            bpc_changes[0].args[0].dbrow_get("flagID"),
            Some(&PyRep::Integer(InventoryFlag::Locked as i64))
        );
        assert_eq!(bpc_changes[0].args[0].dbrow_get("quantity"), Some(&PyRep::Integer(-2)));
        assert!(change_dict_contains(
            &bpc_changes[0].args[1],
            4,
            InventoryFlag::Hangar as i64
        ));
        if switch_on {
            for n in &bpc_notifs {
                shadow.apply_notification(&n.event, &n.args);
            }
            shadow.assert_converged_with_world(&ctx.db, Some(&ctx.sde));
        }

        // 3. Corp BPO ME research job (CorpSAG2 -> Locked 63) -> installer + corp peer notified
        let _ = ctx.drain_notifications_for_char(peer_char_id);
        ctx.dispatch_rpc(
            &mut sess,
            "industryManager",
            None,
            "InstallJob",
            &[PyRep::keyval([
                ("blueprintID", PyRep::Integer(corp_bpo_id)),
                ("blueprintTypeID", PyRep::Integer(688)),
                ("facilityID", PyRep::Integer(station_id)),
                ("activityID", PyRep::Integer(4)),
                ("runs", PyRep::Integer(1)),
                ("useCorp", PyRep::Bool(true)),
            ])],
            &kw,
            None,
        )
        .await
        .expect("corp BPO ME InstallJob");

        let inst_corp_notifs = ctx.drain_notifications_for_char(char_id);
        let peer_corp_notifs = ctx.drain_notifications_for_char(peer_char_id);
        let inst_corp_changes: Vec<_> = inst_corp_notifs
            .iter()
            .filter(|n| {
                n.event == "OnItemChange"
                    && n.args.first().and_then(|r| r.dbrow_get("itemID")).and_then(|v| v.as_i64())
                        == Some(corp_bpo_id)
            })
            .collect();
        assert_eq!(
            inst_corp_changes.len(),
            1,
            "switch_on={switch_on}: installer must receive exactly 1 OnItemChange (corpid only)"
        );
        assert_eq!(inst_corp_changes[0].id_type, "corpid");
        assert_eq!(
            inst_corp_changes[0].args[0].dbrow_get("flagID"),
            Some(&PyRep::Integer(InventoryFlag::Locked as i64))
        );
        assert!(change_dict_contains(
            &inst_corp_changes[0].args[1],
            4,
            InventoryFlag::CorpSAG2 as i64
        ));
        let peer_corp_changes: Vec<_> = peer_corp_notifs
            .iter()
            .filter(|n| {
                n.event == "OnItemChange"
                    && n.args.first().and_then(|r| r.dbrow_get("itemID")).and_then(|v| v.as_i64())
                        == Some(corp_bpo_id)
            })
            .collect();
        assert_eq!(peer_corp_changes.len(), 1);
        assert_eq!(peer_corp_changes[0].id_type, "corpid");
        let inst_bp_updated: Vec<_> = inst_corp_notifs
            .iter()
            .filter(|n| n.event == "OnBlueprintsUpdated")
            .collect();
        let peer_bp_updated: Vec<_> = peer_corp_notifs
            .iter()
            .filter(|n| n.event == "OnBlueprintsUpdated")
            .collect();
        assert_eq!(inst_bp_updated.len(), 1);
        assert_eq!(inst_bp_updated[0].id_type, "ownerID");
        assert_eq!(inst_bp_updated[0].args, vec![PyRep::Integer(corp_id)]);
        assert_eq!(peer_bp_updated.len(), 1);
        assert_eq!(peer_bp_updated[0].id_type, "ownerID");
        assert_eq!(peer_bp_updated[0].args, vec![PyRep::Integer(corp_id)]);

        // 4. Corp station container BPO Copy job (Unlocked 64 -> Locked 63) -> corpid only to installer + peer
        ctx.dispatch_rpc(
            &mut sess,
            "industryManager",
            None,
            "InstallJob",
            &[PyRep::keyval([
                ("blueprintID", PyRep::Integer(corp_cont_bpo_id)),
                ("blueprintTypeID", PyRep::Integer(688)),
                ("facilityID", PyRep::Integer(station_id)),
                ("activityID", PyRep::Integer(5)),
                ("runs", PyRep::Integer(1)),
                ("licensedRuns", PyRep::Integer(1)),
                ("useCorp", PyRep::Bool(true)),
            ])],
            &kw,
            None,
        )
        .await
        .expect("corp container BPO Copy InstallJob");

        let inst_cont_notifs = ctx.drain_notifications_for_char(char_id);
        let peer_cont_notifs = ctx.drain_notifications_for_char(peer_char_id);
        for (who, list) in [("installer", &inst_cont_notifs), ("peer", &peer_cont_notifs)] {
            let cont_changes: Vec<_> = list
                .iter()
                .filter(|n| {
                    n.event == "OnItemChange"
                        && n.args.first().and_then(|r| r.dbrow_get("itemID")).and_then(|v| v.as_i64())
                            == Some(corp_cont_bpo_id)
                })
                .collect();
            assert_eq!(
                cont_changes.len(),
                1,
                "switch_on={switch_on}: {who} must receive 1 OnItemChange for corp container BPO"
            );
            assert_eq!(cont_changes[0].id_type, "corpid");
            assert_eq!(
                cont_changes[0].args[0].dbrow_get("flagID"),
                Some(&PyRep::Integer(InventoryFlag::Locked as i64))
            );
            assert!(change_dict_contains(
                &cont_changes[0].args[1],
                4,
                InventoryFlag::Unlocked as i64
            ));
            let cont_bp_up: Vec<_> = list
                .iter()
                .filter(|n| n.event == "OnBlueprintsUpdated")
                .collect();
            assert_eq!(cont_bp_up.len(), 1);
            assert_eq!(cont_bp_up[0].id_type, "ownerID");
            assert_eq!(cont_bp_up[0].args, vec![PyRep::Integer(corp_id)]);
        }

        let compare = flush_item_compare_summary(&ctx);
        assert_eq!(compare.item_change.hand_written_only, 0);
        assert_eq!(compare.item_change.dictionary_differs, 0);
        assert_eq!(
            compare.item_change.projected_only,
            if switch_on { 0 } else { 1 },
            "switch_on={switch_on}: only unprojected Tritanium consume when switch=0 can be projected_only; compare={compare:#?}"
        );
        assert_eq!(compare.blueprints_updated.hand_written_only, 0);
        assert_eq!(compare.blueprints_updated.projected_only, 0);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn install_job_stacked_blueprint_split_emits_orig_qty_and_locked_split_on_item_change() {
    use crate::router::delta_item_compare::{
        flush_item_compare_summary, set_item_compare_override_for_test,
    };
    use crate::router::ProjectedItemSwitchConfig;
    use eve_testkit::shadow_client::ShadowClient;

    for switch_on in [false, true] {
        let (ctx, char_id, peer_char_id, corp_id, station_id) = make_bp_lock_test_context();
        let stacked_bpo_id = 995_201_i64 + if switch_on { 100 } else { 0 };
        let stacked_bpc_id = stacked_bpo_id + 1;
        let corp_stacked_bpo_id = stacked_bpo_id + 3;
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(
                stacked_bpo_id,
                InventoryItemRecord {
                    item_id: stacked_bpo_id,
                    type_id: 688,
                    owner_id: char_id,
                    location_id: station_id,
                    flag: InventoryFlag::Hangar,
                    quantity: 5,
                    singleton: false,
                    version: 1,
                    ..Default::default()
                },
            );
            snap.items.insert(
                stacked_bpc_id,
                InventoryItemRecord {
                    item_id: stacked_bpc_id,
                    type_id: 688,
                    owner_id: char_id,
                    location_id: station_id,
                    flag: InventoryFlag::Hangar,
                    quantity: 4,
                    singleton: false,
                    is_blueprint_copy: true,
                    blueprint_runs_remaining: Some(5),
                    version: 1,
                    ..Default::default()
                },
            );
            snap.items.insert(
                stacked_bpo_id + 2,
                InventoryItemRecord {
                    item_id: stacked_bpo_id + 2,
                    type_id: 34,
                    owner_id: char_id,
                    location_id: station_id,
                    flag: InventoryFlag::Hangar,
                    quantity: 500,
                    singleton: false,
                    version: 1,
                    ..Default::default()
                },
            );
            snap.items.insert(
                corp_stacked_bpo_id,
                InventoryItemRecord {
                    item_id: corp_stacked_bpo_id,
                    type_id: 688,
                    owner_id: corp_id,
                    location_id: station_id,
                    flag: InventoryFlag::CorpSAG2,
                    quantity: 3,
                    singleton: false,
                    version: 1,
                    ..Default::default()
                },
            );
        });

        ctx.set_projected_item_switches(ProjectedItemSwitchConfig {
            char_same_scene_item_mutation: switch_on,
            char_same_scene_item_deleted: switch_on,
            char_same_scene_item_outside_rpc: false,
            char_same_scene_item_fitted: false,
            char_all_items: false,
        });
        set_item_compare_override_for_test(&ctx, Some(true), Some(None));

        let mut sess = char_session(char_id, corp_id);
        sess.session_id = 1602;
        sess.user_id = Some(ctx.db.with_snapshot(|s| s.characters[&char_id].user_id));
        let kw = HashMap::new();
        let _ = ctx.drain_notifications_for_char(char_id);

        let mut shadow = ShadowClient::from_world(char_id, &ctx.db, Some(&ctx.sde));
        shadow.assert_converged_with_world(&ctx.db, Some(&ctx.sde));

        // 1. Stacked non-singleton BPO (qty 5 -> 4 + new Locked split item) on Copy (0 materials)
        let copy_rep = ctx
            .dispatch_rpc(
                &mut sess,
                "industryManager",
                None,
                "InstallJob",
                &[PyRep::keyval([
                    ("blueprintID", PyRep::Integer(stacked_bpo_id)),
                    ("blueprintTypeID", PyRep::Integer(688)),
                    ("facilityID", PyRep::Integer(station_id)),
                    ("activityID", PyRep::Integer(5)),
                    ("runs", PyRep::Integer(1)),
                    ("licensedRuns", PyRep::Integer(1)),
                ])],
                &kw,
                None,
            )
            .await
            .expect("stacked BPO split InstallJob");
        let split_bpo_id = copy_rep
            .dict_get("blueprintID")
            .and_then(|v| v.as_i64())
            .expect("split blueprintID");
        assert_ne!(split_bpo_id, stacked_bpo_id);

        ctx.db.with_snapshot(|snap| {
            let orig = &snap.items[&stacked_bpo_id];
            assert_eq!(orig.quantity, 4);
            assert_eq!(orig.flag, InventoryFlag::Hangar);
            let split = &snap.items[&split_bpo_id];
            assert_eq!(split.quantity, 1);
            assert!(split.singleton);
            assert_eq!(split.flag, InventoryFlag::Locked);
        });

        let copy_notifs = ctx.drain_notifications_for_char(char_id);
        let orig_changes: Vec<_> = copy_notifs
            .iter()
            .filter(|n| {
                n.event == "OnItemChange"
                    && n.args.first().and_then(|r| r.dbrow_get("itemID")).and_then(|v| v.as_i64())
                        == Some(stacked_bpo_id)
            })
            .collect();
        assert_eq!(
            orig_changes.len(),
            1,
            "switch_on={switch_on}: original stacked BPO must emit exactly 1 OnItemChange (deduped when switch=true)"
        );
        assert_eq!(
            orig_changes[0].args[0].dbrow_get("flagID"),
            Some(&PyRep::Integer(InventoryFlag::Hangar as i64))
        );
        assert_eq!(orig_changes[0].args[0].dbrow_get("quantity"), Some(&PyRep::Integer(4)));
        assert!(change_dict_contains(&orig_changes[0].args[1], 9, 5));
        assert!(!change_dict_has_key(&orig_changes[0].args[1], 5));

        let split_changes: Vec<_> = copy_notifs
            .iter()
            .filter(|n| {
                n.event == "OnItemChange"
                    && n.args.first().and_then(|r| r.dbrow_get("itemID")).and_then(|v| v.as_i64())
                        == Some(split_bpo_id)
            })
            .collect();
        assert_eq!(
            split_changes.len(),
            1,
            "switch_on={switch_on}: split Locked BPO must emit creation OnItemChange"
        );
        assert_eq!(
            split_changes[0].args[0].dbrow_get("flagID"),
            Some(&PyRep::Integer(InventoryFlag::Locked as i64))
        );
        assert!(change_dict_contains(&split_changes[0].args[1], 3, 0));

        // ShadowClient matches world before introducing synthetic non-singleton BPC
        for n in &copy_notifs {
            shadow.apply_notification(&n.event, &n.args);
        }
        shadow.assert_converged_with_world(&ctx.db, Some(&ctx.sde));

        // 2. Stacked BPC split (qty 4 -> 3 + new Locked split BPC)
        let mfg_rep = ctx
            .dispatch_rpc(
                &mut sess,
                "industryManager",
                None,
                "InstallJob",
                &[PyRep::keyval([
                    ("blueprintID", PyRep::Integer(stacked_bpc_id)),
                    ("blueprintTypeID", PyRep::Integer(688)),
                    ("facilityID", PyRep::Integer(station_id)),
                    ("activityID", PyRep::Integer(1)),
                    ("runs", PyRep::Integer(1)),
                ])],
                &kw,
                None,
            )
            .await
            .expect("stacked BPC split InstallJob");
        let split_bpc_id = mfg_rep
            .dict_get("blueprintID")
            .and_then(|v| v.as_i64())
            .expect("split BPC blueprintID");
        assert_ne!(split_bpc_id, stacked_bpc_id);

        let bpc_notifs = ctx.drain_notifications_for_char(char_id);
        let orig_bpc_changes: Vec<_> = bpc_notifs
            .iter()
            .filter(|n| {
                n.event == "OnItemChange"
                    && n.args.first().and_then(|r| r.dbrow_get("itemID")).and_then(|v| v.as_i64())
                        == Some(stacked_bpc_id)
            })
            .collect();
        assert_eq!(orig_bpc_changes.len(), 1);
        assert!(change_dict_contains(&orig_bpc_changes[0].args[1], 9, 4));
        assert!(!change_dict_has_key(&orig_bpc_changes[0].args[1], 5));

        let split_bpc_changes: Vec<_> = bpc_notifs
            .iter()
            .filter(|n| {
                n.event == "OnItemChange"
                    && n.args.first().and_then(|r| r.dbrow_get("itemID")).and_then(|v| v.as_i64())
                        == Some(split_bpc_id)
            })
            .collect();
        assert_eq!(split_bpc_changes.len(), 1);
        assert_eq!(
            split_bpc_changes[0].args[0].dbrow_get("flagID"),
            Some(&PyRep::Integer(InventoryFlag::Locked as i64))
        );
        assert!(change_dict_contains(&split_bpc_changes[0].args[1], 3, 0));

        // 3. Corp stacked BPO split (CorpSAG2 qty 3 -> 2 + new Locked split BPO) -> corpid only to installer + peer
        let _ = ctx.drain_notifications_for_char(char_id);
        let _ = ctx.drain_notifications_for_char(peer_char_id);
        let corp_split_rep = ctx
            .dispatch_rpc(
                &mut sess,
                "industryManager",
                None,
                "InstallJob",
                &[PyRep::keyval([
                    ("blueprintID", PyRep::Integer(corp_stacked_bpo_id)),
                    ("blueprintTypeID", PyRep::Integer(688)),
                    ("facilityID", PyRep::Integer(station_id)),
                    ("activityID", PyRep::Integer(5)),
                    ("runs", PyRep::Integer(1)),
                    ("licensedRuns", PyRep::Integer(1)),
                    ("useCorp", PyRep::Bool(true)),
                ])],
                &kw,
                None,
            )
            .await
            .expect("corp stacked BPO split InstallJob");
        let split_corp_bpo_id = corp_split_rep
            .dict_get("blueprintID")
            .and_then(|v| v.as_i64())
            .expect("split corp BPO blueprintID");
        assert_ne!(split_corp_bpo_id, corp_stacked_bpo_id);

        let inst_corp_split_notifs = ctx.drain_notifications_for_char(char_id);
        let peer_corp_split_notifs = ctx.drain_notifications_for_char(peer_char_id);
        for (who, list) in [
            ("installer", &inst_corp_split_notifs),
            ("peer", &peer_corp_split_notifs),
        ] {
            let corp_orig_changes: Vec<_> = list
                .iter()
                .filter(|n| {
                    n.event == "OnItemChange"
                        && n.args.first().and_then(|r| r.dbrow_get("itemID")).and_then(|v| v.as_i64())
                            == Some(corp_stacked_bpo_id)
                })
                .collect();
            assert_eq!(
                corp_orig_changes.len(),
                1,
                "switch_on={switch_on}: {who} must receive 1 corpid OnItemChange for orig corp stacked BPO"
            );
            assert_eq!(corp_orig_changes[0].id_type, "corpid");
            assert_eq!(
                corp_orig_changes[0].args[0].dbrow_get("flagID"),
                Some(&PyRep::Integer(InventoryFlag::CorpSAG2 as i64))
            );
            assert_eq!(
                corp_orig_changes[0].args[0].dbrow_get("quantity"),
                Some(&PyRep::Integer(2))
            );
            assert!(change_dict_contains(&corp_orig_changes[0].args[1], 9, 3));

            let corp_split_changes: Vec<_> = list
                .iter()
                .filter(|n| {
                    n.event == "OnItemChange"
                        && n.args.first().and_then(|r| r.dbrow_get("itemID")).and_then(|v| v.as_i64())
                            == Some(split_corp_bpo_id)
                })
                .collect();
            assert_eq!(
                corp_split_changes.len(),
                1,
                "switch_on={switch_on}: {who} must receive 1 corpid OnItemChange for split Locked corp BPO"
            );
            assert_eq!(corp_split_changes[0].id_type, "corpid");
            assert_eq!(
                corp_split_changes[0].args[0].dbrow_get("flagID"),
                Some(&PyRep::Integer(InventoryFlag::Locked as i64))
            );
            assert!(change_dict_contains(&corp_split_changes[0].args[1], 3, 0));

            let bp_updated: Vec<_> = list
                .iter()
                .filter(|n| n.event == "OnBlueprintsUpdated")
                .collect();
            assert_eq!(bp_updated.len(), 1);
            assert_eq!(bp_updated[0].id_type, "ownerID");
            assert_eq!(bp_updated[0].args, vec![PyRep::Integer(corp_id)]);
        }

        let compare = flush_item_compare_summary(&ctx);
        assert_eq!(compare.item_change.hand_written_only, 0);
        assert_eq!(compare.item_change.dictionary_differs, 0);
        assert_eq!(
            compare.item_change.projected_only,
            if switch_on { 0 } else { 1 },
            "switch_on={switch_on}: only unprojected Tritanium consume when switch=0 can be projected_only; compare={compare:#?}"
        );
        assert_eq!(compare.blueprints_updated.hand_written_only, 0);
        assert_eq!(compare.blueprints_updated.projected_only, 0);
    }
}

fn seed_bp_lock_item(
    ctx: &ServerContext,
    item_id: i64,
    type_id: eve_sde::TypeId,
    owner_id: i64,
    location_id: i64,
    flag: InventoryFlag,
    quantity: i32,
    singleton: bool,
    runs: Option<i32>,
) {
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            item_id,
            InventoryItemRecord {
                item_id,
                type_id,
                owner_id,
                location_id,
                flag,
                quantity,
                singleton,
                is_blueprint_copy: runs.is_some(),
                blueprint_runs_remaining: runs,
                version: 1,
                ..Default::default()
            },
        );
    });
}

fn on_item_changes_by<'a>(
    notifs: &'a [crate::router::PendingMachoNotification],
    field: &str,
    expected_id: i64,
) -> Vec<&'a crate::router::PendingMachoNotification> {
    notifs
        .iter()
        .filter(|n| {
            n.event == "OnItemChange"
                && n.args.first().and_then(|r| r.dbrow_get(field)).and_then(|v| v.as_i64())
                    == Some(expected_id)
        })
        .collect()
}

fn assert_single_bp_updated(
    notifs: &[crate::router::PendingMachoNotification],
    expected_owner: i64,
    label: &str,
) {
    let bp_up: Vec<_> = notifs
        .iter()
        .filter(|n| n.event == "OnBlueprintsUpdated")
        .collect();
    assert_eq!(bp_up.len(), 1, "{label}: expected 1 OnBlueprintsUpdated");
    assert_eq!(bp_up[0].id_type, "ownerID", "{label}");
    assert_eq!(bp_up[0].args, vec![PyRep::Integer(expected_owner)], "{label}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancel_job_emits_on_item_change_for_unlocked_bp_and_refunded_materials_with_switches_off_and_on() {
    use crate::router::delta_item_compare::{
        flush_item_compare_summary, set_item_compare_override_for_test,
    };
    use crate::router::ProjectedItemSwitchConfig;
    use eve_testkit::shadow_client::ShadowClient;

    for switch_on in [false, true] {
        let (ctx, char_id, peer_char_id, corp_id, station_id) = make_bp_lock_test_context();
        let off = if switch_on { 100 } else { 0 };
        let (bpc_id, bpo_id, mat_id) = (995_301_i64 + off, 995_302_i64 + off, 995_303_i64 + off);
        let (corp_bpo_id, corp_cont_id, corp_cont_bpo_id, corp_mat_id) = (
            995_304_i64 + off,
            995_305_i64 + off,
            995_306_i64 + off,
            995_307_i64 + off,
        );
        ctx.db.with_mut_snapshot(|snap| {
            snap.items
                .retain(|_, i| !(i.owner_id == char_id && i.type_id == 34));
        });
        seed_bp_lock_item(&ctx, bpc_id, 688, char_id, station_id, InventoryFlag::Hangar, 1, true, Some(3));
        seed_bp_lock_item(&ctx, bpo_id, 688, char_id, station_id, InventoryFlag::Hangar, 1, true, None);
        seed_bp_lock_item(&ctx, mat_id, 34, char_id, station_id, InventoryFlag::Hangar, 100, false, None);
        seed_bp_lock_item(&ctx, corp_bpo_id, 688, corp_id, station_id, InventoryFlag::CorpSAG2, 1, true, None);
        seed_bp_lock_item(&ctx, corp_cont_id, 3293, corp_id, station_id, InventoryFlag::CorpSAG2, 1, true, None);
        seed_bp_lock_item(&ctx, corp_cont_bpo_id, 688, corp_id, corp_cont_id, InventoryFlag::Unlocked, 1, true, None);
        seed_bp_lock_item(&ctx, corp_mat_id, 34, corp_id, station_id, InventoryFlag::CorpSAG1, 100, false, None);

        ctx.set_projected_item_switches(ProjectedItemSwitchConfig {
            char_same_scene_item_mutation: switch_on,
            char_same_scene_item_deleted: switch_on,
            char_same_scene_item_outside_rpc: false,
            char_same_scene_item_fitted: false,
            char_all_items: false,
        });
        set_item_compare_override_for_test(&ctx, Some(true), Some(None));

        let mut sess = char_session(char_id, corp_id);
        sess.session_id = 1602;
        sess.user_id = Some(ctx.db.with_snapshot(|s| s.characters[&char_id].user_id));
        let kw = HashMap::new();
        let assert_cancel_compare = |before: crate::router::delta_item_compare::ItemCompareSummary, label: &str| {
            let after = flush_item_compare_summary(&ctx);
            assert_eq!(after.item_change.hand_written_only, before.item_change.hand_written_only, "switch_on={switch_on} {label}: after={after:#?}");
            assert_eq!(after.item_change.projected_only, before.item_change.projected_only, "switch_on={switch_on} {label}: after={after:#?}");
            assert_eq!(after.item_change.dictionary_differs, before.item_change.dictionary_differs, "switch_on={switch_on} {label}: after={after:#?}");
            assert_eq!(after.blueprints_updated.hand_written_only, before.blueprints_updated.hand_written_only, "switch_on={switch_on} {label}: after={after:#?}");
            assert_eq!(after.blueprints_updated.projected_only, before.blueprints_updated.projected_only, "switch_on={switch_on} {label}: after={after:#?}");
        };

        // 1. Personal CancelJob with full consumption -> unlocks BPC (63 -> 4) + creates NEW Tritanium stack
        let j1 = ctx
            .dispatch_rpc(
                &mut sess,
                "industryManager",
                None,
                "InstallJob",
                &[PyRep::keyval([
                    ("blueprintID", PyRep::Integer(bpc_id)),
                    ("blueprintTypeID", PyRep::Integer(688)),
                    ("facilityID", PyRep::Integer(station_id)),
                    ("activityID", PyRep::Integer(1)),
                    ("runs", PyRep::Integer(1)),
                ])],
                &kw,
                None,
            )
            .await
            .unwrap()
            .dict_get("jobID")
            .and_then(|v| v.as_i64())
            .unwrap();
        let _ = ctx.drain_notifications_for_char(char_id);
        let compare_before_1 = flush_item_compare_summary(&ctx);
        let mut shadow = ShadowClient::from_world(char_id, &ctx.db, Some(&ctx.sde));
        shadow.assert_converged_with_world(&ctx.db, Some(&ctx.sde));

        ctx.dispatch_rpc(&mut sess, "industryManager", None, "CancelJob", &[PyRep::Integer(j1)], &kw, None)
            .await
            .expect("personal CancelJob new stack");
        let c1_notifs = ctx.drain_notifications_for_char(char_id);
        let c1_bp = on_item_changes_by(&c1_notifs, "itemID", bpc_id);
        assert_eq!(c1_bp.len(), 1);
        assert_eq!(c1_bp[0].id_type, "charid");
        assert_eq!(c1_bp[0].args[0].dbrow_get("flagID"), Some(&PyRep::Integer(InventoryFlag::Hangar as i64)));
        assert!(change_dict_contains(&c1_bp[0].args[1], 4, InventoryFlag::Locked as i64));
        let c1_refund = on_item_changes_by(&c1_notifs, "typeID", 34);
        assert_eq!(c1_refund.len(), 1);
        assert_eq!(c1_refund[0].args[0].dbrow_get("quantity"), Some(&PyRep::Integer(100)));
        assert!(change_dict_contains(&c1_refund[0].args[1], 3, 0));
        assert_single_bp_updated(&c1_notifs, char_id, "personal CancelJob");
        for n in &c1_notifs {
            shadow.apply_notification(&n.event, &n.args);
        }
        shadow.assert_converged_with_world(&ctx.db, Some(&ctx.sde));
        assert_cancel_compare(compare_before_1, "personal new stack");

        // 2. Personal CancelJob merging into an existing Hangar Tritanium stack (50 -> 150)
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.values_mut().find(|i| i.owner_id == char_id && i.type_id == 34).unwrap().quantity = 150;
        });
        let j2 = ctx
            .dispatch_rpc(
                &mut sess,
                "industryManager",
                None,
                "InstallJob",
                &[PyRep::keyval([
                    ("blueprintID", PyRep::Integer(bpo_id)),
                    ("blueprintTypeID", PyRep::Integer(688)),
                    ("facilityID", PyRep::Integer(station_id)),
                    ("activityID", PyRep::Integer(1)),
                    ("runs", PyRep::Integer(1)),
                ])],
                &kw,
                None,
            )
            .await
            .unwrap()
            .dict_get("jobID")
            .and_then(|v| v.as_i64())
            .unwrap();
        let _ = ctx.drain_notifications_for_char(char_id);
        let compare_before_2 = flush_item_compare_summary(&ctx);
        let mut shadow2 = ShadowClient::from_world(char_id, &ctx.db, Some(&ctx.sde));

        ctx.dispatch_rpc(&mut sess, "industryManager", None, "CancelJob", &[PyRep::Integer(j2)], &kw, None)
            .await
            .expect("personal CancelJob merged stack");
        let c2_notifs = ctx.drain_notifications_for_char(char_id);
        let c2_refund = on_item_changes_by(&c2_notifs, "typeID", 34);
        assert_eq!(c2_refund.len(), 1);
        assert_eq!(c2_refund[0].args[0].dbrow_get("quantity"), Some(&PyRep::Integer(150)));
        assert!(change_dict_contains(&c2_refund[0].args[1], 9, 50));
        for n in &c2_notifs {
            shadow2.apply_notification(&n.event, &n.args);
        }
        shadow2.assert_converged_with_world(&ctx.db, Some(&ctx.sde));
        assert_cancel_compare(compare_before_2, "personal merged stack");

        // 3. Corp CancelJob: unlocks CorpSAG2 BPO (63 -> 117), creates new CorpSAG1 Tritanium stack -> installer + peer corpid
        let cj1 = ctx
            .dispatch_rpc(
                &mut sess,
                "industryManager",
                None,
                "InstallJob",
                &[PyRep::keyval([
                    ("blueprintID", PyRep::Integer(corp_bpo_id)),
                    ("blueprintTypeID", PyRep::Integer(688)),
                    ("facilityID", PyRep::Integer(station_id)),
                    ("activityID", PyRep::Integer(1)),
                    ("runs", PyRep::Integer(1)),
                    ("useCorp", PyRep::Bool(true)),
                    ("inputFlag", PyRep::Integer(InventoryFlag::CorpSAG1 as i64)),
                ])],
                &kw,
                None,
            )
            .await
            .unwrap()
            .dict_get("jobID")
            .and_then(|v| v.as_i64())
            .unwrap();
        let _ = ctx.drain_notifications_for_char(char_id);
        let _ = ctx.drain_notifications_for_char(peer_char_id);
        let compare_before_3 = flush_item_compare_summary(&ctx);

        ctx.dispatch_rpc(&mut sess, "industryManager", None, "CancelJob", &[PyRep::Integer(cj1)], &kw, None)
            .await
            .expect("corp CancelJob new stack");
        for (who, list) in [("installer", ctx.drain_notifications_for_char(char_id)), ("peer", ctx.drain_notifications_for_char(peer_char_id))] {
            let bp_changes = on_item_changes_by(&list, "itemID", corp_bpo_id);
            assert_eq!(bp_changes.len(), 1, "{who} corp BP unlock");
            assert_eq!(bp_changes[0].id_type, "corpid");
            assert_eq!(bp_changes[0].args[0].dbrow_get("flagID"), Some(&PyRep::Integer(InventoryFlag::CorpSAG2 as i64)));
            assert!(change_dict_contains(&bp_changes[0].args[1], 4, InventoryFlag::Locked as i64));
            let mat_changes = on_item_changes_by(&list, "typeID", 34);
            assert_eq!(mat_changes.len(), 1, "{who} corp Tritanium new refund");
            assert_eq!(mat_changes[0].id_type, "corpid");
            assert_eq!(mat_changes[0].args[0].dbrow_get("flagID"), Some(&PyRep::Integer(InventoryFlag::CorpSAG1 as i64)));
            assert!(change_dict_contains(&mat_changes[0].args[1], 3, 0));
            assert_single_bp_updated(&list, corp_id, who);
        }
        assert_cancel_compare(compare_before_3, "corp new stack");

        // 4. Corp container CancelJob: unlocks 63 -> Unlocked (64) + merges CorpSAG1 Tritanium (50 -> 150)
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.values_mut().find(|i| i.owner_id == corp_id && i.type_id == 34).unwrap().quantity = 150;
        });
        let cj2 = ctx
            .dispatch_rpc(
                &mut sess,
                "industryManager",
                None,
                "InstallJob",
                &[PyRep::keyval([
                    ("blueprintID", PyRep::Integer(corp_cont_bpo_id)),
                    ("blueprintTypeID", PyRep::Integer(688)),
                    ("facilityID", PyRep::Integer(station_id)),
                    ("activityID", PyRep::Integer(1)),
                    ("runs", PyRep::Integer(1)),
                    ("useCorp", PyRep::Bool(true)),
                    ("inputFlag", PyRep::Integer(InventoryFlag::CorpSAG1 as i64)),
                ])],
                &kw,
                None,
            )
            .await
            .unwrap()
            .dict_get("jobID")
            .and_then(|v| v.as_i64())
            .unwrap();
        let _ = ctx.drain_notifications_for_char(char_id);
        let _ = ctx.drain_notifications_for_char(peer_char_id);
        let compare_before_4 = flush_item_compare_summary(&ctx);

        ctx.dispatch_rpc(&mut sess, "industryManager", None, "CancelJob", &[PyRep::Integer(cj2)], &kw, None)
            .await
            .expect("corp container CancelJob merged stack");
        for (who, list) in [("installer", ctx.drain_notifications_for_char(char_id)), ("peer", ctx.drain_notifications_for_char(peer_char_id))] {
            let cont_bp = on_item_changes_by(&list, "itemID", corp_cont_bpo_id);
            assert_eq!(cont_bp.len(), 1, "{who} corp container BP unlock");
            assert_eq!(cont_bp[0].id_type, "corpid");
            assert_eq!(cont_bp[0].args[0].dbrow_get("flagID"), Some(&PyRep::Integer(InventoryFlag::Unlocked as i64)));
            assert!(change_dict_contains(&cont_bp[0].args[1], 4, InventoryFlag::Locked as i64));
            let corp_merge = on_item_changes_by(&list, "typeID", 34);
            assert_eq!(corp_merge.len(), 1, "{who} corp merged Tritanium");
            assert_eq!(corp_merge[0].id_type, "corpid");
            assert_eq!(corp_merge[0].args[0].dbrow_get("quantity"), Some(&PyRep::Integer(150)));
            assert!(change_dict_contains(&corp_merge[0].args[1], 9, 50));
            assert_single_bp_updated(&list, corp_id, who);
        }

        assert_cancel_compare(compare_before_4, "corp container merged stack");
    }
}

#[cfg(test)]
#[path = "industry_jobs_extra_unit_tests.rs"]
mod extra_unit_tests;
