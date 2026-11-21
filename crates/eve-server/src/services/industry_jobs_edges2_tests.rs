//! Additional edge-case and branch coverage tests for `services::industry_jobs`.

use super::*;
use crate::router::ServerContext;
use crate::services::inventory_fitting::fitting_containers_ships::set_container_password_snap;
use eve_db::{
    CorporationRecord, InventoryFlag, InventoryItemRecord, IskCents,
    TrainedSkillRecord, UpwellStructureRecord, WorldRepository,
};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::{
    BlueprintActivity, BlueprintInfo, BlueprintMaterial, BlueprintProduct, BlueprintSkill,
    InvType, SdeRegistry, SolarSystem,
};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

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

fn setup_ctx_and_sde() -> ServerContext {
    let mut sde = SdeRegistry::with_canonical_seed();
    sde.types.insert(34, make_inv_type(34, 18, "Tritanium", 0.01, 2.0));
    sde.types.insert(587, make_inv_type(587, 25, "Rifter", 27289.0, 250_000.0));
    sde.types.insert(688, make_inv_type(688, 105, "Rifter Blueprint", 0.01, 1_000_000.0));
    sde.types.insert(20424, make_inv_type(20424, 333, "Datacore - Mechanical Engineering", 0.1, 100_000.0));
    sde.types.insert(34201, make_inv_type(34201, 1304, "Accelerant Decryptor", 0.1, 500_000.0));
    sde.types.insert(11372, make_inv_type(11372, 105, "Jaguar Blueprint", 0.01, 5_000_000.0));
    sde.types.insert(90274, make_inv_type(90274, 1889, "Unrefined Reaction Formula", 0.01, 100_000.0));
    sde.types.insert(16670, make_inv_type(16670, 428, "Crystalline Carbonide", 0.1, 10_000.0));
    // Lowsec system for reaction checks
    sde.solar_systems.insert(
        30_002_187,
        SolarSystem {
            solar_system_id: 30_002_187,
            name: "Tama".into(),
            region_id: 10_000_030,
            constellation_id: 20_000_322,
            security_status: 0.3,
            radius: 1e12,
            galactic_pos_ly: [0.0; 3],
            planet_ids: vec![],
            stations: vec![],
            wormhole_class_id: None,
        },
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
            invention_base_probability: 0.34,
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
                        skills: vec![BlueprintSkill { type_id: 3380, level: 3 }], // Requires Industry 3
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
                            probability: Some(0.34),
                        }],
                        skills: vec![BlueprintSkill { type_id: 21790, level: 1 }], // Requires Caldari/Minmatar Encryption Methods 1
                    },
                ),
            ]),
        },
    );
    sde.blueprints.insert(
        90274,
        BlueprintInfo {
            blueprint_type_id: 90274,
            product_type_id: 16670,
            product_quantity: 100,
            max_production_limit: 100,
            manufacturing_time_sec: 180,
            research_me_time_sec: 0,
            research_te_time_sec: 0,
            copy_time_sec: 0,
            invention_time_sec: 0,
            invention_base_probability: 0.0,
            materials: vec![],
            activities: HashMap::from([(
                "reaction".into(),
                BlueprintActivity {
                    time: 180,
                    materials: vec![BlueprintMaterial {
                        type_id: 34,
                        quantity: 500,
                    }],
                    products: vec![BlueprintProduct {
                        type_id: 16670,
                        quantity: 100,
                        probability: None,
                    }],
                    skills: vec![BlueprintSkill { type_id: 45746, level: 1 }], // Reactions 1
                },
            )]),
        },
    );

    ServerContext::with_sde_and_repository(Arc::new(sde), Arc::new(WorldRepository::with_demo_seed()))
        .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED)
}

fn char_session(char_id: i64, corp_id: i64, station_id: Option<i64>, structure_id: Option<i64>) -> SessionState {
    SessionState {
        user_id: Some(1),
        character_id: Some(char_id),
        corporation_id: Some(corp_id),
        solar_system_id: Some(30_000_142),
        station_id,
        structure_id,
        corp_role: 0,
        ..Default::default()
    }
}

#[test]
fn install_job_param_remote_range_and_reaction_facility_guards() {
    let ctx = setup_ctx_and_sde();
    let char_id = 96_900_001_i64;
    let corp_id = 98_950_i64;
    let station_id = 60_003_760_i64;
    let athanor_hs = 10_000_990_001_i64;
    let athanor_ls = 10_000_990_002_i64;
    let bpo_id = 96_900_100_i64;
    let rxn_bp_id = 96_900_101_i64;
    let kw = HashMap::new();

    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().cloned().expect("char");
        ch.character_id = char_id;
        ch.corporation_id = corp_id;
        ch.station_id = Some(station_id);
        ch.balance = IskCents::from_isk(100_000_000);
        snap.characters.insert(char_id, ch);
        snap.skills.entry(char_id).or_default().insert(3380, TrainedSkillRecord { type_id: 3380, level: 5, skill_points: 256_000.0 });

        snap.items.insert(bpo_id, make_item(bpo_id, 688, char_id, station_id, InventoryFlag::Hangar, 1, true));
        snap.items.insert(rxn_bp_id, make_item(rxn_bp_id, 90274, char_id, athanor_ls, InventoryFlag::Hangar, 1, true));

        snap.structures.insert(
            athanor_hs,
            UpwellStructureRecord {
                structure_id: athanor_hs,
                type_id: 35835, // Athanor
                owner_corp_id: corp_id,
                solar_system_id: 30_000_142, // highsec
                name: "Highsec Athanor".into(),
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 100,
                ..Default::default()
            },
        );
        snap.structures.insert(
            athanor_ls,
            UpwellStructureRecord {
                structure_id: athanor_ls,
                type_id: 35835, // Athanor (only Standup Composite Reactor vs Hybrid?)
                owner_corp_id: corp_id,
                solar_system_id: 30_002_187, // lowsec 0.3
                name: "Lowsec Athanor".into(),
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 100,
                ..Default::default()
            },
        );
    });

    // 1. NotDocked when neither station nor structure
    let mut space_sess = char_session(char_id, corp_id, None, None);
    assert_eq!(
        handle_industry_jobs(
            &ctx,
            &mut space_sess,
            "industryManager",
            "InstallJob",
            &[PyRep::keyval([("blueprintID", PyRep::Integer(bpo_id)), ("activityID", PyRep::Integer(1)), ("runs", PyRep::Integer(1))])],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "NotDocked"
    );

    let mut sess = char_session(char_id, corp_id, Some(station_id), None);

    // 2. Missing blueprint params, bad runs, bad activity, licensedRuns on non-copy, MaxRunsPerCopyExceeded, UnknownBlueprint
    assert_eq!(
        handle_industry_jobs(&ctx, &mut sess, "industryManager", "InstallJob", &[], &kw)
            .unwrap()
            .unwrap_err()
            .code,
        "InvalidIndustryJobParameters"
    );
    assert_eq!(
        handle_industry_jobs(
            &ctx,
            &mut sess,
            "industryManager",
            "InstallJob",
            &[PyRep::keyval([("blueprintID", PyRep::Integer(bpo_id)), ("activityID", PyRep::Integer(1)), ("runs", PyRep::Integer(0))])],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidIndustryJobParameters"
    );
    assert_eq!(
        handle_industry_jobs(
            &ctx,
            &mut sess,
            "industryManager",
            "InstallJob",
            &[PyRep::keyval([("blueprintID", PyRep::Integer(bpo_id)), ("activityID", PyRep::Integer(7)), ("runs", PyRep::Integer(1))])],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidIndustryJobParameters"
    );
    // #964: non-copying activity (activityID=1) ignores V24 licensedRuns and leaves job.licensed_runs = None
    let temp_bpo_id = 96_900_199_i64;
    let temp_trit_id = 96_900_198_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            temp_trit_id,
            make_item(temp_trit_id, 34, char_id, station_id, InventoryFlag::Hangar, 1_000, false),
        );
        snap.items.insert(
            temp_bpo_id,
            make_item(temp_bpo_id, 688, char_id, station_id, InventoryFlag::Hangar, 1, true),
        );
    });
    let non_copy_rep = handle_industry_jobs(
        &ctx,
        &mut sess,
        "industryManager",
        "InstallJob",
        &[PyRep::keyval([
            ("blueprintID", PyRep::Integer(temp_bpo_id)),
            ("activityID", PyRep::Integer(1)),
            ("runs", PyRep::Integer(1)),
            ("licensedRuns", PyRep::Integer(5)),
        ])],
        &kw,
    )
    .unwrap()
    .expect("non-copying activity with licensedRuns=5 must succeed");
    let non_copy_jid = non_copy_rep.dict_get("jobID").unwrap().as_i64().unwrap();
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.industry_jobs[&non_copy_jid].licensed_runs, None);
    });
    handle_industry_jobs(
        &ctx,
        &mut sess,
        "industryManager",
        "CancelJob",
        &[PyRep::Integer(non_copy_jid)],
        &kw,
    )
    .unwrap()
    .expect("cancel scratch non-copying job");
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.remove(&temp_bpo_id);
        snap.items.retain(|_, i| i.type_id != 34);
    });
    assert_eq!(
        handle_industry_jobs(
            &ctx,
            &mut sess,
            "industryManager",
            "InstallJob",
            &[PyRep::keyval([
                ("blueprintID", PyRep::Integer(bpo_id)),
                ("activityID", PyRep::Integer(5)),
                ("runs", PyRep::Integer(1)),
                ("licensedRuns", PyRep::Integer(999)),
            ])],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "MaxRunsPerCopyExceeded"
    );

    // 3. FacilityNotFound & FacilityOutOfRemoteRange
    assert_eq!(
        handle_industry_jobs(
            &ctx,
            &mut sess,
            "industryManager",
            "InstallJob",
            &[PyRep::keyval([
                ("blueprintID", PyRep::Integer(bpo_id)),
                ("facilityID", PyRep::Integer(99_999_999)),
                ("activityID", PyRep::Integer(1)),
                ("runs", PyRep::Integer(1)),
            ])],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "FacilityNotFound"
    );
    let remote_facility = ctx
        .sde
        .stations
        .values()
        .find(|s| s.solar_system_id != 30_000_142)
        .map(|s| s.station_id)
        .unwrap_or(athanor_ls);
    assert_eq!(
        handle_industry_jobs(
            &ctx,
            &mut sess,
            "industryManager",
            "InstallJob",
            &[PyRep::keyval([
                ("blueprintID", PyRep::Integer(bpo_id)),
                ("facilityID", PyRep::Integer(remote_facility)),
                ("activityID", PyRep::Integer(1)),
                ("runs", PyRep::Integer(1)),
            ])],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "FacilityOutOfRemoteRange"
    );

    // 4. Reactions facility / sec / reactor / formula / skill guards
    assert_eq!(
        handle_industry_jobs(
            &ctx,
            &mut sess,
            "industryManager",
            "InstallJob",
            &[PyRep::keyval([
                ("blueprintID", PyRep::Integer(bpo_id)),
                ("facilityID", PyRep::Integer(station_id)),
                ("activityID", PyRep::Integer(9)),
                ("runs", PyRep::Integer(1)),
            ])],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "FacilityDoesNotSupportReactions"
    );

    let mut hs_ath_sess = char_session(char_id, corp_id, None, Some(athanor_hs));
    assert_eq!(
        handle_industry_jobs(
            &ctx,
            &mut hs_ath_sess,
            "industryManager",
            "InstallJob",
            &[PyRep::keyval([
                ("blueprintID", PyRep::Integer(bpo_id)),
                ("facilityID", PyRep::Integer(athanor_hs)),
                ("activityID", PyRep::Integer(9)),
                ("runs", PyRep::Integer(1)),
            ])],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "ReactionsForbiddenInHighSec"
    );

    // In lowsec Athanor: without Composite Reactor service -> CompositeReactorRequired (or MissingReactionSkills/BlueprintNotReactionFormula)
    let mut ls_ath_sess = char_session(char_id, corp_id, None, Some(athanor_ls));
    let rxn_err = handle_industry_jobs(
        &ctx,
        &mut ls_ath_sess,
        "industryManager",
        "InstallJob",
        &[PyRep::keyval([
            ("blueprintID", PyRep::Integer(rxn_bp_id)),
            ("facilityID", PyRep::Integer(athanor_ls)),
            ("activityID", PyRep::Integer(9)),
            ("runs", PyRep::Integer(1)),
        ])],
        &kw,
    )
    .unwrap()
    .unwrap_err();
    assert!(
        matches!(rxn_err.code.as_str(), "CompositeReactorRequired" | "FacilityServiceOffline"),
        "unexpected error code: {}",
        rxn_err.code
    );

    // Non-reaction blueprint in reaction activity -> BlueprintNotReactionFormula (once Athanor has Composite Reactor fitted & online)
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(char_id).or_default().insert(
            45746, // Reactions
            TrainedSkillRecord {
                type_id: 45746,
                level: 1,
                skill_points: 250.0,
            },
        );
        snap.module_online_states.insert(96_900_190, true);
        snap.items.insert(
            96_900_190,
            InventoryItemRecord {
                item_id: 96_900_190,
                type_id: 45537, // Standup Composite Reactor I
                owner_id: corp_id,
                location_id: athanor_ls,
                flag: InventoryFlag::ServiceSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(96_900_191, make_item(96_900_191, 688, char_id, athanor_ls, InventoryFlag::Hangar, 1, true));
    });
    assert_eq!(
        handle_industry_jobs(
            &ctx,
            &mut ls_ath_sess,
            "industryManager",
            "InstallJob",
            &[PyRep::keyval([
                ("blueprintID", PyRep::Integer(96_900_191)),
                ("facilityID", PyRep::Integer(athanor_ls)),
                ("activityID", PyRep::Integer(9)),
                ("runs", PyRep::Integer(1)),
            ])],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "BlueprintNotReactionFormula"
    );
}

#[test]
fn install_job_corp_roles_container_locks_bpc_invention_skills_materials_funds_and_split() {
    let ctx = setup_ctx_and_sde();
    let char_id = 96_900_002_i64;
    let corp_id = 98_951_i64;
    let station_id = 60_003_760_i64;
    let bpo_stack_id = 96_900_200_i64;
    let bpc_id = 96_900_201_i64;
    let locked_bp_id = 96_900_202_i64;
    let pw_cont_id = 96_900_203_i64;
    let bp_in_pw_cont = 96_900_204_i64;
    let maxed_bpo_id = 96_900_205_i64;
    let corp_bpo_id = 96_900_206_i64;
    let kw = HashMap::new();

    ctx.db.with_mut_snapshot(|snap| {
        let mut corp = { let mut __corp = CorporationRecord::default(); __corp.corporation_id = corp_id; __corp.name = "IndEdgeCorp".into(); __corp.ticker = "IEC".into(); __corp.ceo_id = char_id + 10; __corp.members = HashSet::from([char_id]); let __mw = IskCents::from_isk(100); if __mw.0 != 0 { __corp.set_division_balance(1, __mw); } else { __corp.ensure_wallet_divisions(); } __corp };
        corp.ensure_wallet_divisions();
        corp.set_division_balance(1, IskCents::from_isk(100));
        snap.corporations.insert(corp_id, corp);

        let mut ch = snap.characters.values().next().cloned().expect("char");
        ch.character_id = char_id;
        ch.corporation_id = corp_id;
        ch.station_id = Some(station_id);
        ch.balance = IskCents::from_isk(10); // Low balance initially
        snap.characters.insert(char_id, ch);

        snap.items.insert(bpo_stack_id, make_item(bpo_stack_id, 688, char_id, station_id, InventoryFlag::Hangar, 5, false));
        let mut bpc_item = make_item(bpc_id, 688, char_id, station_id, InventoryFlag::Hangar, 1, true);
        bpc_item.is_blueprint_copy = true;
        bpc_item.blueprint_runs_remaining = Some(2);
        bpc_item.dynamic_attributes.insert(ATTR_BLUEPRINT_ME, 10.0);
        bpc_item.dynamic_attributes.insert(ATTR_BLUEPRINT_TE, 20.0);
        snap.items.insert(bpc_id, bpc_item);
        snap.items.insert(locked_bp_id, make_item(locked_bp_id, 688, char_id, station_id, InventoryFlag::Locked, 1, true));
        snap.items.insert(pw_cont_id, make_item(pw_cont_id, 23, char_id, station_id, InventoryFlag::Hangar, 1, true));
        snap.items.insert(bp_in_pw_cont, make_item(bp_in_pw_cont, 688, char_id, pw_cont_id, InventoryFlag::Hangar, 1, true));
        let mut maxed_item = make_item(maxed_bpo_id, 688, char_id, station_id, InventoryFlag::Hangar, 1, true);
        maxed_item.is_blueprint_copy = false;
        maxed_item.dynamic_attributes.insert(ATTR_BLUEPRINT_ME, 10.0);
        maxed_item.dynamic_attributes.insert(ATTR_BLUEPRINT_TE, 20.0);
        snap.items.insert(maxed_bpo_id, maxed_item);
        snap.items.insert(corp_bpo_id, make_item(corp_bpo_id, 688, corp_id, station_id, InventoryFlag::CorpSAG1, 1, true));

        set_container_password_snap(snap, pw_cont_id, 1, "secret", 1000).unwrap();
    });

    let mut sess = char_session(char_id, corp_id, Some(station_id), None);

    // 1. Corp job without factory roles -> CrpAccessDenied
    assert_eq!(
        handle_industry_jobs(
            &ctx,
            &mut sess,
            "industryManager",
            "InstallJob",
            &[PyRep::keyval([
                ("blueprintID", PyRep::Integer(corp_bpo_id)),
                ("facilityID", PyRep::Integer(station_id)),
                ("activityID", PyRep::Integer(1)),
                ("runs", PyRep::Integer(1)),
                ("ownerID", PyRep::Integer(corp_id)),
            ])],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "CrpAccessDenied"
    );

    // 2. Locked BP & password-locked container BP -> BlueprintNotAvailable
    assert_eq!(
        handle_industry_jobs(
            &ctx,
            &mut sess,
            "industryManager",
            "InstallJob",
            &[PyRep::keyval([("blueprintID", PyRep::Integer(locked_bp_id)), ("activityID", PyRep::Integer(1)), ("runs", PyRep::Integer(1))])],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "BlueprintNotAvailable"
    );
    assert_eq!(
        handle_industry_jobs(
            &ctx,
            &mut sess,
            "industryManager",
            "InstallJob",
            &[PyRep::keyval([("blueprintID", PyRep::Integer(bp_in_pw_cont)), ("activityID", PyRep::Integer(1)), ("runs", PyRep::Integer(1))])],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "BlueprintNotAvailable"
    );

    // 3. CannotResearchOrCopyBPC, InsufficientBlueprintRuns, InventionRequiresBPC, MaxTime/MaterialEfficiencyReached
    assert_eq!(
        handle_industry_jobs(
            &ctx,
            &mut sess,
            "industryManager",
            "InstallJob",
            &[PyRep::keyval([("blueprintID", PyRep::Integer(bpc_id)), ("activityID", PyRep::Integer(4)), ("runs", PyRep::Integer(1))])],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "CannotResearchOrCopyBPC"
    );
    assert_eq!(
        handle_industry_jobs(
            &ctx,
            &mut sess,
            "industryManager",
            "InstallJob",
            &[PyRep::keyval([("blueprintID", PyRep::Integer(bpc_id)), ("activityID", PyRep::Integer(1)), ("runs", PyRep::Integer(5))])],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InsufficientBlueprintRuns"
    );
    assert_eq!(
        handle_industry_jobs(
            &ctx,
            &mut sess,
            "industryManager",
            "InstallJob",
            &[PyRep::keyval([("blueprintID", PyRep::Integer(maxed_bpo_id)), ("activityID", PyRep::Integer(8)), ("runs", PyRep::Integer(1))])],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InventionRequiresBPC"
    );
    assert_eq!(
        handle_industry_jobs(
            &ctx,
            &mut sess,
            "industryManager",
            "InstallJob",
            &[PyRep::keyval([("blueprintID", PyRep::Integer(maxed_bpo_id)), ("activityID", PyRep::Integer(3)), ("runs", PyRep::Integer(1))])],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "MaxTimeEfficiencyReached"
    );
    assert_eq!(
        handle_industry_jobs(
            &ctx,
            &mut sess,
            "industryManager",
            "InstallJob",
            &[PyRep::keyval([("blueprintID", PyRep::Integer(maxed_bpo_id)), ("activityID", PyRep::Integer(4)), ("runs", PyRep::Integer(1))])],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "MaxMaterialEfficiencyReached"
    );

    // 4. MissingInventionSkills -> add skill -> InsufficientInventionDatacores -> add datacore -> InsufficientDecryptors
    assert_eq!(
        handle_industry_jobs(
            &ctx,
            &mut sess,
            "industryManager",
            "InstallJob",
            &[PyRep::keyval([("blueprintID", PyRep::Integer(bpc_id)), ("activityID", PyRep::Integer(8)), ("runs", PyRep::Integer(1))])],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "MissingInventionSkills"
    );
    ctx.db.with_mut_snapshot(|snap| {
        let entry = snap.skills.entry(char_id).or_default();
        for sid in [11448_u32, 11452_u32, 21790_u32, 21791_u32] {
            entry.insert(sid, TrainedSkillRecord { type_id: sid, level: 3, skill_points: 50_000.0 });
        }
    });
    assert_eq!(
        handle_industry_jobs(
            &ctx,
            &mut sess,
            "industryManager",
            "InstallJob",
            &[PyRep::keyval([("blueprintID", PyRep::Integer(bpc_id)), ("activityID", PyRep::Integer(8)), ("runs", PyRep::Integer(1))])],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InsufficientInventionDatacores"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(96_900_210, make_item(96_900_210, 20424, char_id, station_id, InventoryFlag::Hangar, 10, false));
    });
    assert_eq!(
        handle_industry_jobs(
            &ctx,
            &mut sess,
            "industryManager",
            "InstallJob",
            &[PyRep::keyval([
                ("blueprintID", PyRep::Integer(bpc_id)),
                ("activityID", PyRep::Integer(8)),
                ("runs", PyRep::Integer(1)),
                ("decryptorTypeID", PyRep::Integer(34201)),
            ])],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InsufficientDecryptors"
    );

    // 5. InsufficientFunds -> top up -> MissingIndustrySkills -> add skill -> InsufficientManufacturingMaterials -> InvalidInputContainer / InvalidOutputLocation -> BPO stack split success
    assert_eq!(
        handle_industry_jobs(
            &ctx,
            &mut sess,
            "industryManager",
            "InstallJob",
            &[PyRep::keyval([("blueprintID", PyRep::Integer(bpo_stack_id)), ("activityID", PyRep::Integer(1)), ("runs", PyRep::Integer(1))])],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InsufficientFunds"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().balance = IskCents::from_isk(100_000_000);
    });
    assert_eq!(
        handle_industry_jobs(
            &ctx,
            &mut sess,
            "industryManager",
            "InstallJob",
            &[PyRep::keyval([("blueprintID", PyRep::Integer(bpo_stack_id)), ("activityID", PyRep::Integer(1)), ("runs", PyRep::Integer(1))])],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "MissingIndustrySkills"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(char_id).or_default().insert(3380, TrainedSkillRecord { type_id: 3380, level: 5, skill_points: 256_000.0 });
    });
    assert_eq!(
        handle_industry_jobs(
            &ctx,
            &mut sess,
            "industryManager",
            "InstallJob",
            &[PyRep::keyval([("blueprintID", PyRep::Integer(bpo_stack_id)), ("activityID", PyRep::Integer(1)), ("runs", PyRep::Integer(1))])],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InsufficientManufacturingMaterials"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(96_900_211, make_item(96_900_211, 34, char_id, station_id, InventoryFlag::Hangar, 5000, false));
    });

    assert_eq!(
        handle_industry_jobs(
            &ctx,
            &mut sess,
            "industryManager",
            "InstallJob",
            &[PyRep::keyval([
                ("blueprintID", PyRep::Integer(bpo_stack_id)),
                ("activityID", PyRep::Integer(1)),
                ("runs", PyRep::Integer(1)),
                ("inputLocationID", PyRep::Integer(88_888_888)),
            ])],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidInputContainer"
    );
    assert_eq!(
        handle_industry_jobs(
            &ctx,
            &mut sess,
            "industryManager",
            "InstallJob",
            &[PyRep::keyval([
                ("blueprintID", PyRep::Integer(bpo_stack_id)),
                ("activityID", PyRep::Integer(1)),
                ("runs", PyRep::Integer(1)),
                ("outputLocationID", PyRep::Integer(88_888_888)),
            ])],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidOutputLocation"
    );

    // BPO stack split (qty 5 -> 4 remaining + 1 locked stack)
    let res = handle_industry_jobs(
        &ctx,
        &mut sess,
        "industryManager",
        "InstallJob",
        &[PyRep::keyval([
            ("blueprintID", PyRep::Integer(bpo_stack_id)),
            ("activityID", PyRep::Integer(1)),
            ("runs", PyRep::Integer(1)),
        ])],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert!(res.dict_get("jobID").is_some());
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items[&bpo_stack_id].quantity, 4);
    });
}

#[test]
fn install_job_invention_rejects_runs_exceeding_bpc_licensed_runs_without_mutation() {
    let ctx = setup_ctx_and_sde();
    let char_id = 96_910_001_i64;
    let corp_id = 98_952_i64;
    let station_id = 60_003_760_i64;
    let bpo_id = 96_910_100_i64;
    let bpc_id = 96_910_101_i64;
    let datacore_id = 96_910_102_i64;
    let kw = HashMap::new();

    ctx.db.with_mut_snapshot(|snap| {
        let mut corp = CorporationRecord::default();
        corp.corporation_id = corp_id;
        corp.name = "InvRunsCorp".into();
        corp.ticker = "IRC".into();
        corp.ceo_id = char_id;
        corp.members = HashSet::from([char_id]);
        corp.ensure_wallet_divisions();
        snap.corporations.insert(corp_id, corp);

        let mut ch = snap.characters.values().next().cloned().expect("char");
        ch.character_id = char_id;
        ch.corporation_id = corp_id;
        ch.station_id = Some(station_id);
        ch.balance = IskCents::from_isk(50_000_000);
        snap.characters.insert(char_id, ch);

        let sk = snap.skills.entry(char_id).or_default();
        sk.insert(21790, TrainedSkillRecord { type_id: 21790, level: 5, skill_points: 256_000.0 });
        sk.insert(3380, TrainedSkillRecord { type_id: 3380, level: 5, skill_points: 256_000.0 });

        snap.items.insert(bpo_id, make_item(bpo_id, 688, char_id, station_id, InventoryFlag::Hangar, 1, true));
        let mut bpc = make_item(bpc_id, 688, char_id, station_id, InventoryFlag::Hangar, 1, true);
        bpc.is_blueprint_copy = true;
        bpc.blueprint_runs_remaining = Some(2);
        snap.items.insert(bpc_id, bpc);
        snap.items.insert(datacore_id, make_item(datacore_id, 20424, char_id, station_id, InventoryFlag::Hangar, 100, false));
    });

    let mut sess = char_session(char_id, corp_id, Some(station_id), None);

    // Explicit BPO must still return InventionRequiresBPC.
    let err_bpo = handle_industry_jobs(
        &ctx,
        &mut sess,
        "industryManager",
        "InstallJob",
        &[PyRep::keyval([
            ("blueprintID", PyRep::Integer(bpo_id)),
            ("activityID", PyRep::Integer(8)),
            ("runs", PyRep::Integer(1)),
        ])],
        &kw,
    )
    .unwrap()
    .unwrap_err();
    assert_eq!(err_bpo.code, "InventionRequiresBPC");

    // 2-run BPC requested with runs = 5 must reject BEFORE snapshot mutation.
    let err_runs = handle_industry_jobs(
        &ctx,
        &mut sess,
        "industryManager",
        "InstallJob",
        &[PyRep::keyval([
            ("blueprintID", PyRep::Integer(bpc_id)),
            ("activityID", PyRep::Integer(8)),
            ("runs", PyRep::Integer(5)),
        ])],
        &kw,
    )
    .unwrap()
    .unwrap_err();
    assert_eq!(err_runs.code, "InsufficientBlueprintRuns");

    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.characters[&char_id].balance, IskCents::from_isk(50_000_000));
        assert_eq!(snap.items[&datacore_id].quantity, 100);
        assert_eq!(snap.items[&bpc_id].flag, InventoryFlag::Hangar);
        assert_eq!(snap.items[&bpc_id].blueprint_runs_remaining, Some(2));
        assert!(snap.industry_jobs.is_empty());
    });
}

#[test]
fn complete_corp_job_enforces_factory_manager_role_and_complete_many_jobs_is_atomic() {
    let ctx = setup_ctx_and_sde();
    let clock = Arc::new(eve_db::clock::VirtualServerClock::with_fixed_base(1_776_000_000));
    ctx.set_clock(clock.clone());

    let installer_id = 96_920_001_i64;
    let member_id = 96_920_002_i64;
    let manager_id = 96_920_003_i64;
    let ceo_id = 96_920_099_i64;
    let corp_id = 98_953_i64;
    let station_id = 60_003_760_i64;
    let corp_bpo_id = 96_920_100_i64;
    let personal_bpo1 = 96_920_101_i64;
    let personal_bpo2 = 96_920_102_i64;
    let corp_install_roles = eve_db::corp_roles::ROLE_FACTORY_MANAGER
        | (1u64 << 13)
        | eve_db::corp_roles::ROLE_ACCOUNT_CAN_TAKE[0];
    let kw = HashMap::new();

    ctx.db.with_mut_snapshot(|snap| {
        let mut corp = CorporationRecord::default();
        corp.corporation_id = corp_id;
        corp.name = "CorpDeliveryGate".into();
        corp.ticker = "CDG".into();
        corp.ceo_id = ceo_id;
        corp.members = HashSet::from([ceo_id, installer_id, member_id, manager_id]);
        corp.member_roles.insert(installer_id, corp_install_roles);
        corp.member_roles.insert(manager_id, eve_db::corp_roles::ROLE_FACTORY_MANAGER);
        corp.member_roles.insert(member_id, 0);
        corp.ensure_wallet_divisions();
        corp.set_division_balance(1, IskCents::from_isk(50_000_000));
        snap.corporations.insert(corp_id, corp);

        let template = snap.characters.values().next().cloned().expect("char");
        for cid in [installer_id, member_id, manager_id, ceo_id] {
            let mut ch = template.clone();
            ch.character_id = cid;
            ch.corporation_id = corp_id;
            ch.station_id = Some(station_id);
            ch.balance = IskCents::from_isk(50_000_000);
            snap.characters.insert(cid, ch);
            let sk = snap.skills.entry(cid).or_default();
            for sid in [3380, 3387] {
                sk.insert(sid, TrainedSkillRecord { type_id: sid, level: 5, skill_points: 256_000.0 });
            }
        }

        snap.items.insert(corp_bpo_id, make_item(corp_bpo_id, 688, corp_id, station_id, InventoryFlag::CorpSAG1, 1, true));
        snap.items.insert(96_920_200, make_item(96_920_200, 34, corp_id, station_id, InventoryFlag::CorpSAG1, 10_000, false));
        snap.items.insert(personal_bpo1, make_item(personal_bpo1, 688, installer_id, station_id, InventoryFlag::Hangar, 1, true));
        snap.items.insert(personal_bpo2, make_item(personal_bpo2, 688, installer_id, station_id, InventoryFlag::Hangar, 1, true));
        snap.items.insert(96_920_201, make_item(96_920_201, 34, installer_id, station_id, InventoryFlag::Hangar, 20_000, false));
    });

    let mut inst_sess = char_session(installer_id, corp_id, Some(station_id), None);
    inst_sess.corp_role = corp_install_roles;
    let corp_job_rep = handle_industry_jobs(
        &ctx,
        &mut inst_sess,
        "industryManager",
        "InstallJob",
        &[PyRep::keyval([
            ("blueprintID", PyRep::Integer(corp_bpo_id)),
            ("activityID", PyRep::Integer(1)),
            ("runs", PyRep::Integer(1)),
            ("ownerID", PyRep::Integer(corp_id)),
            ("inputFlag", PyRep::Integer(InventoryFlag::CorpSAG1 as i64)),
        ])],
        &kw,
    )
    .unwrap()
    .unwrap();
    let corp_job_id = corp_job_rep.dict_get("jobID").unwrap().as_i64().unwrap();

    // Advance enough for corp_job_id (600s) to finish.
    eve_db::clock::ServerClock::advance_secs(&*clock, 1_000);

    // Ordinary 0-role non-installer member in same corp must be rejected.
    let mut member_sess = char_session(member_id, corp_id, Some(station_id), None);
    let err_member = handle_industry_jobs(
        &ctx,
        &mut member_sess,
        "industryManager",
        "CompleteJob",
        &[PyRep::Integer(corp_job_id)],
        &kw,
    )
    .unwrap()
    .unwrap_err();
    assert_eq!(err_member.code, "JobOwnershipViolation");
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.industry_jobs[&corp_job_id].status, "Active");
    });

    // Factory Manager in same corp succeeds.
    let mut mgr_sess = char_session(manager_id, corp_id, Some(station_id), None);
    mgr_sess.corp_role = eve_db::corp_roles::ROLE_FACTORY_MANAGER;
    let mgr_ok = handle_industry_jobs(
        &ctx,
        &mut mgr_sess,
        "industryManager",
        "CompleteJob",
        &[PyRep::Integer(corp_job_id)],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(mgr_ok.dict_get("jobID").and_then(|v| v.as_i64()), Some(corp_job_id));
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.industry_jobs[&corp_job_id].status, "Delivered");
    });

    // Now test CompleteManyJobs rollback atomicity across 2 personal jobs.
    let j1 = handle_industry_jobs(
        &ctx,
        &mut inst_sess,
        "industryManager",
        "InstallJob",
        &[PyRep::keyval([
            ("blueprintID", PyRep::Integer(personal_bpo1)),
            ("activityID", PyRep::Integer(1)),
            ("runs", PyRep::Integer(1)),
        ])],
        &kw,
    )
    .unwrap()
    .unwrap()
    .dict_get("jobID")
    .unwrap()
    .as_i64()
    .unwrap();
    let j2 = handle_industry_jobs(
        &ctx,
        &mut inst_sess,
        "industryManager",
        "InstallJob",
        &[PyRep::keyval([
            ("blueprintID", PyRep::Integer(personal_bpo2)),
            ("activityID", PyRep::Integer(1)),
            ("runs", PyRep::Integer(5)),
        ])],
        &kw,
    )
    .unwrap()
    .unwrap()
    .dict_get("jobID")
    .unwrap()
    .as_i64()
    .unwrap();

    let rifter_count_before = ctx.db.with_snapshot(|snap| {
        snap.items.values().filter(|it| it.type_id == 587).count()
    });
    // Advance 800s: j1 (600s) is ready, j2 (3000s) is NOT ready yet.
    eve_db::clock::ServerClock::advance_secs(&*clock, 800);
    let err_batch = handle_industry_jobs(
        &ctx,
        &mut inst_sess,
        "industryManager",
        "CompleteManyJobs",
        &[PyRep::List(vec![PyRep::Integer(j1), PyRep::Integer(j2)])],
        &kw,
    )
    .unwrap()
    .unwrap_err();
    assert_eq!(err_batch.code, "JobNotYetComplete");

    // Atomic rollback: j1 was NOT delivered, BPO1 stays Locked, zero new 587 Rifters minted.
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.industry_jobs[&j1].status, "Active");
        assert_eq!(snap.industry_jobs[&j2].status, "Active");
        assert_eq!(snap.items[&personal_bpo1].flag, InventoryFlag::Locked);
        assert_eq!(
            snap.items.values().filter(|it| it.type_id == 587).count(),
            rifter_count_before
        );
        assert_eq!(
            snap.items
                .values()
                .filter(|it| it.owner_id == installer_id && it.type_id == 587)
                .count(),
            0
        );
    });
}
