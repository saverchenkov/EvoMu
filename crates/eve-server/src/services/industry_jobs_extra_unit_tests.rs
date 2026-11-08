//! Extra unit and mutation-killing tests for `services::industry_pi::industry_jobs`.

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::industry_pi::industry_jobs::handle_industry_jobs;
use crate::services::industry_pi::ATTR_BLUEPRINT_TE;
use eve_db::types::UpwellStructureRecord;
use eve_db::{
    corp_roles, CorpWalletDivisionRecord, CorporationRecord, InventoryFlag, InventoryItemRecord,
    IskCents, TrainedSkillRecord, WorldRepository,
};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::{
    BlueprintActivity, BlueprintInfo, BlueprintMaterial, BlueprintProduct, InvType, SdeRegistry,
};
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

fn make_inv_type(type_id: u32, group_id: u32, name: &str, base_price: f64) -> InvType {
    InvType {
        type_id,
        group_id,
        category_id: 9,
        name: name.into(),
        mass: 1.0,
        volume: 1.0,
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
        version: 1,
        ..Default::default()
    }
}

fn make_ctx_and_session() -> (ServerContext, SessionState, i64, i64, i64) {
    let mut sde = (*shared_sde()).clone();
    sde.types.insert(34, make_inv_type(34, 18, "Tritanium", 2.0));
    sde.types.insert(587, make_inv_type(587, 25, "Rifter", 250_000.0));
    sde.types.insert(688, make_inv_type(688, 105, "Rifter Blueprint", 1_000_000.0));
    sde.types.insert(30_752, make_inv_type(30_752, 971, "Intact Hull Section", 5_000_000.0));
    let mut dec_t = make_inv_type(34_201, 1304, "Accelerant Decryptor", 1_000_000.0);
    dec_t.default_attributes = HashMap::from([(1112, 1.2), (1113, 2.0), (1114, 10.0), (1124, 1.0)]);
    sde.types.insert(34_201, dec_t);
    sde.types.insert(90_274, make_inv_type(90_274, 1888, "Unrefined Reaction Formula", 100_000.0));
    sde.types.insert(46_166, make_inv_type(46_166, 1890, "Biochemical Reaction Formula", 100_000.0));
    sde.types.insert(16_670, make_inv_type(16_670, 428, "Crystalline Carbonide", 1_000.0));

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
            materials: vec![BlueprintMaterial { type_id: 34, quantity: 100 }],
            activities: HashMap::from([
                (
                    "manufacturing".into(),
                    BlueprintActivity {
                        time: 600,
                        materials: vec![BlueprintMaterial { type_id: 34, quantity: 100 }],
                        products: vec![BlueprintProduct { type_id: 587, quantity: 1, probability: None }],
                        skills: vec![],
                    },
                ),
                (
                    "invention".into(),
                    BlueprintActivity {
                        time: 200,
                        materials: vec![],
                        products: vec![BlueprintProduct { type_id: 11_372, quantity: 1, probability: None }],
                        skills: vec![],
                    },
                ),
            ]),
        },
    );
    sde.blueprints.insert(
        30_752,
        BlueprintInfo {
            blueprint_type_id: 30_752,
            product_type_id: 29_984,
            product_quantity: 1,
            max_production_limit: 10,
            manufacturing_time_sec: 0,
            research_me_time_sec: 0,
            research_te_time_sec: 0,
            copy_time_sec: 0,
            invention_time_sec: 300,
            invention_base_probability: 1.0,
            materials: vec![],
            activities: HashMap::from([(
                "8".into(),
                BlueprintActivity {
                    time: 300,
                    materials: vec![BlueprintMaterial { type_id: 34, quantity: 10 }],
                    products: vec![BlueprintProduct { type_id: 29_984, quantity: 3, probability: None }],
                    skills: vec![],
                },
            )]),
        },
    );
    for (bp_tid, act_key) in [(90_274u32, "reaction"), (46_166u32, "9")] {
        sde.blueprints.insert(
            bp_tid,
            BlueprintInfo {
                blueprint_type_id: bp_tid,
                product_type_id: 16_670,
                product_quantity: 100,
                max_production_limit: 100,
                manufacturing_time_sec: 0,
                research_me_time_sec: 0,
                research_te_time_sec: 0,
                copy_time_sec: 0,
                invention_time_sec: 0,
                invention_base_probability: 0.0,
                materials: vec![],
                activities: HashMap::from([(
                    act_key.into(),
                    BlueprintActivity {
                        time: 3_600,
                        materials: vec![BlueprintMaterial { type_id: 34, quantity: 50 }],
                        products: vec![BlueprintProduct { type_id: 16_670, quantity: 100, probability: None }],
                        skills: vec![],
                    },
                )]),
            },
        );
    }

    let ctx = ServerContext::with_sde_and_repository(
        Arc::new(sde),
        Arc::new(WorldRepository::with_demo_seed()),
    );
    let char_id = 90_000_001i64;
    let corp_id = 98_600_001i64;
    let station_id = 60_003_760i64;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.corporation_id = corp_id;
            ch.station_id = Some(station_id);
            ch.solar_system_id = 30_000_142;
            ch.balance = IskCents::from_f64_isk(1_000_000_000.0);
        }
        snap.corporations.insert(
            corp_id,
            { let mut __corp = CorporationRecord::default();
                __corp.corporation_id = corp_id;
                __corp.name = "Industry Test Corp".into();
                __corp.ticker = "INDT".into();
                __corp.ceo_id = 90_000_999;
                __corp.wallet_divisions = vec![CorpWalletDivisionRecord {
                    division_id: 1,
                    name: "Master".into(),
                    balance: IskCents::from_f64_isk(1_000_000_000.0),
                }];
                let __mw = IskCents::from_f64_isk(1_000_000_000.0);
                if __mw.0 != 0 { __corp.set_division_balance(1, __mw); } else { __corp.ensure_wallet_divisions(); }
                __corp },
        );
        let skills = snap.skills.entry(char_id).or_default();
        for sid in [3380, 3387, 3406, 11448, 11452, 21791, 45746, 45748] {
            skills.insert(sid, TrainedSkillRecord { type_id: sid, level: 5, skill_points: 256_000.0 });
        }
    });

    let mut sess = SessionState::default();
    sess.user_id = Some(1);
    sess.character_id = Some(char_id);
    sess.corporation_id = Some(corp_id);
    sess.station_id = Some(station_id);
    sess.solar_system_id = Some(30_000_142);
    (ctx, sess, char_id, corp_id, station_id)
}

#[test]
fn reactions_lowsec_composite_reactor_ancient_relic_and_decryptor_invention() {
    let (ctx, mut sess, char_id, corp_id, station_id) = make_ctx_and_session();
    let kw = HashMap::new();

    let lowsec_sys = ctx
        .sde
        .solar_systems
        .values()
        .find(|s| s.solar_system_id < 30_100_000 && s.security_status < 0.40)
        .unwrap()
        .solar_system_id;
    let highsec_athanor = 1_045_000_001i64;
    let lowsec_athanor = 1_045_000_002i64;

    ctx.db.with_mut_snapshot(|snap| {
        for (sid, sys) in [(highsec_athanor, 30_000_142), (lowsec_athanor, lowsec_sys)] {
            snap.structures.insert(
                sid,
                UpwellStructureRecord {
                    structure_id: sid,
                    type_id: 35_835,
                    owner_corp_id: corp_id,
                    solar_system_id: sys,
                    name: format!("Athanor {sid}"),
                    state: "ShieldVulnerable".into(),
                    quantum_core_installed: true,
                    fuel_blocks: 500,
                    ..Default::default()
                },
            );
        }
        snap.items.insert(70_001, make_item(70_001, 45_539, corp_id, lowsec_athanor, InventoryFlag::ServiceSlot0, 1, true));
        snap.module_online_states.insert(70_001, true);
        snap.items.insert(70_010, make_item(70_010, 46_166, char_id, station_id, InventoryFlag::Hangar, 1, true));
        snap.items.insert(70_011, make_item(70_011, 46_166, char_id, highsec_athanor, InventoryFlag::Hangar, 1, true));
        snap.items.insert(70_012, make_item(70_012, 90_274, char_id, lowsec_athanor, InventoryFlag::Hangar, 1, true));
        snap.items.insert(70_013, make_item(70_013, 46_166, char_id, lowsec_athanor, InventoryFlag::Hangar, 1, true));
        snap.items.insert(70_014, make_item(70_014, 34, char_id, lowsec_athanor, InventoryFlag::Hangar, 5_000, false));
    });

    // 1. Reaction at NPC station -> FacilityDoesNotSupportReactions
    assert_eq!(
        handle_industry_jobs(&ctx, &mut sess, "industryManager", "InstallJob", &[PyRep::Integer(46_166), PyRep::Integer(9), PyRep::Integer(1)], &kw)
            .unwrap()
            .unwrap_err()
            .code,
        "FacilityDoesNotSupportReactions"
    );

    // 2. Reaction at High-sec Athanor -> ReactionsForbiddenInHighSec
    sess.station_id = None;
    sess.structure_id = Some(highsec_athanor);
    assert_eq!(
        handle_industry_jobs(&ctx, &mut sess, "industryManager", "InstallJob", &[PyRep::Integer(46_166), PyRep::Integer(9), PyRep::Integer(1)], &kw)
            .unwrap()
            .unwrap_err()
            .code,
        "ReactionsForbiddenInHighSec"
    );

    // 3. Unrefined mineral reaction (90274) at Low-sec Athanor without 45537 -> CompositeReactorRequired
    sess.structure_id = Some(lowsec_athanor);
    sess.solar_system_id = Some(lowsec_sys);
    assert_eq!(
        handle_industry_jobs(&ctx, &mut sess, "industryManager", "InstallJob", &[PyRep::Integer(90_274), PyRep::Integer(9), PyRep::Integer(1)], &kw)
            .unwrap()
            .unwrap_err()
            .code,
        "CompositeReactorRequired"
    );

    // 4. Normal reaction (46166) at Low-sec Athanor succeeds and completes!
    let react_rep = handle_industry_jobs(
        &ctx,
        &mut sess,
        "industryManager",
        "InstallJob",
        &[PyRep::Dict(vec![
            (PyRep::String("blueprintTypeID".into()), PyRep::Integer(46_166)),
            (PyRep::String("activityID".into()), PyRep::Integer(9)),
            (PyRep::String("runs".into()), PyRep::Integer(2)),
        ])],
        &kw,
    )
    .unwrap()
    .unwrap();
    let react_jid = react_rep.dict_get("jobID").unwrap().as_i64().unwrap();
    ctx.db.with_mut_snapshot(|snap| {
        snap.industry_jobs.get_mut(&react_jid).unwrap().start_unix = 0;
    });
    let react_done = handle_industry_jobs(&ctx, &mut sess, "industryManager", "CompleteJob", &[PyRep::Integer(react_jid)], &kw)
        .unwrap()
        .unwrap();
    assert!(react_done.dict_get("deliveredItemID").is_some());

    // 5. Ancient Relic (30752, group 971, quantity 2, !is_blueprint_copy) + Decryptor (34201) invention at station_id
    sess.structure_id = None;
    sess.station_id = Some(station_id);
    sess.solar_system_id = Some(30_000_142);
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(70_020, make_item(70_020, 30_752, char_id, station_id, InventoryFlag::Hangar, 2, false));
        snap.items.insert(70_021, make_item(70_021, 34, char_id, station_id, InventoryFlag::Hangar, 1_000, false));
        snap.items.insert(70_022, make_item(70_022, 34_201, char_id, station_id, InventoryFlag::Hangar, 1, false));
    });
    let relic_rep = handle_industry_jobs(
        &ctx,
        &mut sess,
        "industryManager",
        "InstallJob",
        &[PyRep::keyval([
            ("blueprintItemID", PyRep::Integer(70_020)),
            ("blueprintTypeID", PyRep::Integer(30_752)),
            ("activityID", PyRep::Integer(8)),
            ("runs", PyRep::Integer(1)),
            ("decryptorTypeID", PyRep::Integer(34_201)),
        ])],
        &kw,
    )
    .unwrap()
    .unwrap();
    let relic_jid = relic_rep.dict_get("jobID").unwrap().as_i64().unwrap();
    assert_eq!(relic_rep.dict_get("inventedRuns").and_then(|v| v.as_i64()), Some(4));
    ctx.db.with_mut_snapshot(|snap| {
        snap.industry_jobs.get_mut(&relic_jid).unwrap().start_unix = 0;
    });
    handle_industry_jobs(&ctx, &mut sess, "industryManager", "CompleteJob", &[PyRep::Integer(relic_jid)], &kw)
        .unwrap()
        .unwrap();
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items[&70_020].quantity, 1);
        assert_eq!(snap.items[&70_020].flag, InventoryFlag::Hangar);
    });
}

#[test]
fn corp_industry_jobs_remote_range_bpc_restrictions_te_research_and_cancel_errors() {
    let (ctx, mut sess, char_id, corp_id, station_id) = make_ctx_and_session();
    let kw = HashMap::new();

    // 1. Remote facility range check: facility not found vs out of jump range
    assert_eq!(
        handle_industry_jobs(
            &ctx,
            &mut sess,
            "industryManager",
            "InstallJob",
            &[PyRep::keyval([
                ("blueprintTypeID", PyRep::Integer(688)),
                ("activityID", PyRep::Integer(1)),
                ("runs", PyRep::Integer(1)),
                ("facilityID", PyRep::Integer(69_999_999)),
            ])],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "FacilityNotFound"
    );

    let remote_station_id = ctx
        .sde
        .stations
        .values()
        .find(|st| st.solar_system_id != 30_000_142)
        .map(|st| st.station_id)
        .unwrap();
    assert_eq!(
        handle_industry_jobs(
            &ctx,
            &mut sess,
            "industryManager",
            "InstallJob",
            &[PyRep::keyval([
                ("blueprintTypeID", PyRep::Integer(688)),
                ("activityID", PyRep::Integer(1)),
                ("runs", PyRep::Integer(1)),
                ("facilityID", PyRep::Integer(remote_station_id)),
            ])],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "FacilityOutOfRemoteRange"
    );

    // 2. BPC restrictions: CannotResearchOrCopyBPC, InventionRequiresBPC, InsufficientBlueprintRuns
    ctx.db.with_mut_snapshot(|snap| {
        let mut bpc = make_item(80_001, 688, char_id, station_id, InventoryFlag::Hangar, 1, true);
        bpc.is_blueprint_copy = true;
        bpc.blueprint_runs_remaining = Some(1);
        snap.items.insert(80_001, bpc);
    });
    assert_eq!(
        handle_industry_jobs(&ctx, &mut sess, "industryManager", "InstallJob", &[PyRep::Integer(688), PyRep::Integer(3), PyRep::Integer(1)], &kw)
            .unwrap()
            .unwrap_err()
            .code,
        "CannotResearchOrCopyBPC"
    );
    assert_eq!(
        handle_industry_jobs(&ctx, &mut sess, "industryManager", "InstallJob", &[PyRep::Integer(688), PyRep::Integer(1), PyRep::Integer(5)], &kw)
            .unwrap()
            .unwrap_err()
            .code,
        "InsufficientBlueprintRuns"
    );

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.remove(&80_001);
        snap.items.insert(80_002, make_item(80_002, 688, char_id, station_id, InventoryFlag::Hangar, 1, true));
    });
    assert_eq!(
        handle_industry_jobs(&ctx, &mut sess, "industryManager", "InstallJob", &[PyRep::Integer(688), PyRep::Integer(8), PyRep::Integer(1)], &kw)
            .unwrap()
            .unwrap_err()
            .code,
        "InventionRequiresBPC"
    );

    // 3. Activity 3 (TE research) install & complete -> increases ATTR_BLUEPRINT_TE by 2.0 * runs
    let te_rep = handle_industry_jobs(&ctx, &mut sess, "industryManager", "InstallJob", &[PyRep::Integer(688), PyRep::Integer(3), PyRep::Integer(2)], &kw)
        .unwrap()
        .unwrap();
    let te_jid = te_rep.dict_get("jobID").unwrap().as_i64().unwrap();
    ctx.db.with_mut_snapshot(|snap| {
        snap.industry_jobs.get_mut(&te_jid).unwrap().start_unix = 0;
    });
    handle_industry_jobs(&ctx, &mut sess, "industryManager", "CompleteJob", &[PyRep::Integer(te_jid)], &kw)
        .unwrap()
        .unwrap();
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items[&80_002].dynamic_attributes.get(&ATTR_BLUEPRINT_TE), Some(&4.0));
    });

    // 4. Corporate job permissions, install, and CancelJob material refund to CorpSAG1
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(80_010, make_item(80_010, 688, corp_id, station_id, InventoryFlag::CorpSAG1, 1, true));
        snap.items.insert(80_011, make_item(80_011, 34, corp_id, station_id, InventoryFlag::CorpSAG1, 1_000, false));
    });
    assert_eq!(
        handle_industry_jobs(
            &ctx,
            &mut sess,
            "industryManager",
            "InstallJob",
            &[PyRep::keyval([
                ("blueprintItemID", PyRep::Integer(80_010)),
                ("blueprintTypeID", PyRep::Integer(688)),
                ("activityID", PyRep::Integer(1)),
                ("runs", PyRep::Integer(1)),
                ("useCorp", PyRep::Bool(true)),
            ])],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "CrpAccessDenied"
    );

    sess.corp_role = corp_roles::ROLE_FACTORY_MANAGER | (1u64 << 13) | corp_roles::ROLE_ACCOUNT_CAN_TAKE[0];
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations
            .get_mut(&corp_id)
            .unwrap()
            .member_roles
            .insert(char_id, corp_roles::ROLE_FACTORY_MANAGER | (1u64 << 13) | corp_roles::ROLE_ACCOUNT_CAN_TAKE[0]);
    });
    let corp_job_rep = handle_industry_jobs(
        &ctx,
        &mut sess,
        "industryManager",
        "InstallJob",
        &[PyRep::keyval([
            ("blueprintItemID", PyRep::Integer(80_010)),
            ("blueprintTypeID", PyRep::Integer(688)),
            ("activityID", PyRep::Integer(1)),
            ("runs", PyRep::Integer(2)),
            ("ownerID", PyRep::Integer(corp_id)),
        ])],
        &kw,
    )
    .unwrap()
    .unwrap();
    let corp_jid = corp_job_rep.dict_get("jobID").unwrap().as_i64().unwrap();
    assert_eq!(corp_job_rep.dict_get("ownerID"), Some(&PyRep::Integer(corp_id)));

    // CancelJob on non-existent job, then cancel corp_jid -> refunds materials to CorpSAG1
    assert_eq!(
        handle_industry_jobs(&ctx, &mut sess, "industryManager", "CancelJob", &[PyRep::Integer(999_999)], &kw)
            .unwrap()
            .unwrap_err()
            .code,
        "JobNotFound"
    );
    handle_industry_jobs(&ctx, &mut sess, "industryManager", "CancelJob", &[PyRep::Integer(corp_jid)], &kw)
        .unwrap()
        .unwrap();
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items[&80_011].quantity, 1_000);
    });
    assert_eq!(
        handle_industry_jobs(&ctx, &mut sess, "industryManager", "CancelJob", &[PyRep::Integer(corp_jid)], &kw)
            .unwrap()
            .unwrap_err()
            .code,
        "InvalidJobState"
    );
}
