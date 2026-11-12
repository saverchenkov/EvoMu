//! Deep branch-coverage and mutation-killing unit tests for `services::industry_pi::industry_jobs`.

use super::*;
use crate::router::ServerContext;
use crate::services::industry_pi::{load_job_cost_meta, save_job_cost_meta, ATTR_BLUEPRINT_TE};
use eve_db::types::UpwellStructureRecord;
use eve_db::{
    corp_roles, CorpWalletDivisionRecord, CorporationRecord, InventoryFlag, InventoryItemRecord,
    IskCents, TrainedSkillRecord, WorldRepository,
};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::{
    BlueprintActivity, BlueprintInfo, BlueprintMaterial, BlueprintProduct, InvType, SdeRegistry,
    StationInfo,
};
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

fn mk_type(type_id: u32, group_id: u32, name: &str, base_price: f64) -> InvType {
    InvType {
        type_id,
        group_id,
        category_id: 9,
        name: name.into(),
        mass: 1.0,
        volume: 1.0,
        capacity: 5_000.0,
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

fn mk_item(id: i64, tid: u32, owner: i64, loc: i64, flag: InventoryFlag, qty: i32, single: bool) -> InventoryItemRecord {
    InventoryItemRecord {
        item_id: id,
        type_id: tid,
        owner_id: owner,
        location_id: loc,
        flag,
        quantity: qty,
        singleton: single,
        version: 1,
        ..Default::default()
    }
}

fn build_ctx() -> (ServerContext, SessionState, i64, i64, i64) {
    let mut sde = (*shared_sde()).clone();
    sde.types.insert(34, mk_type(34, 18, "Tritanium", 2.0));
    sde.types.insert(587, mk_type(587, 25, "Rifter", 250_000.0));
    sde.types.insert(688, mk_type(688, 105, "Rifter Blueprint", 1_000_000.0));
    sde.types.insert(689, mk_type(689, 105, "NonInventable BP", 100_000.0));
    sde.types.insert(30_752, mk_type(30_752, 971, "Intact Hull Section", 5_000_000.0));
    let mut dec = mk_type(34_201, 1304, "Accelerant Decryptor", 1_000_000.0);
    dec.default_attributes = HashMap::from([(1112, 1.2), (1113, 2.0), (1114, 10.0), (1124, 1.0)]);
    sde.types.insert(34_201, dec);
    sde.types.insert(90_274, mk_type(90_274, 1888, "Unrefined Reaction", 100_000.0));
    sde.types.insert(46_166, mk_type(46_166, 1890, "Biochem Reaction", 100_000.0));
    sde.types.insert(46_167, mk_type(46_167, 1890, "Empty Reaction", 100_000.0));
    sde.types.insert(16_670, mk_type(16_670, 428, "Carbonide", 1_000.0));

    // Second station in Jita (30_000_142) for same-system remote facility check
    let mut st2: StationInfo = sde.stations[&60_003_760].clone();
    st2.station_id = 60_003_761;
    sde.stations.insert(60_003_761, st2.clone());
    if let Some(jita) = sde.solar_systems.get_mut(&30_000_142) {
        if !jita.stations.iter().any(|s| s.station_id == 60_003_761) {
            jita.stations.push(st2);
        }
    }

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
            // Two Tritanium entries exercise consumed_materials duplicate accumulation
            materials: vec![
                BlueprintMaterial { type_id: 34, quantity: 60 },
                BlueprintMaterial { type_id: 34, quantity: 40 },
            ],
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
                        materials: vec![], // Empty -> triggers fallback [20424, 20172] datacores
                        products: vec![BlueprintProduct { type_id: 11_372, quantity: 1, probability: None }],
                        skills: vec![],
                    },
                ),
            ]),
        },
    );
    sde.blueprints.insert(
        689,
        BlueprintInfo {
            blueprint_type_id: 689,
            product_type_id: 587,
            product_quantity: 1,
            max_production_limit: 5,
            manufacturing_time_sec: 100,
            research_me_time_sec: 105,
            research_te_time_sec: 105,
            copy_time_sec: 100,
            invention_time_sec: 100,
            invention_base_probability: 0.0, // Not inventable!
            materials: vec![],
            activities: HashMap::new(),
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
    for (bp_tid, key, mats) in [
        (90_274_u32, "reaction", vec![BlueprintMaterial { type_id: 34, quantity: 50 }]),
        (46_166_u32, "9", vec![BlueprintMaterial { type_id: 34, quantity: 50 }]),
        (46_167_u32, "reaction", vec![]),
    ] {
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
                    key.into(),
                    BlueprintActivity {
                        time: 3_600,
                        materials: mats,
                        products: vec![BlueprintProduct { type_id: 16_670, quantity: 100, probability: None }],
                        skills: vec![],
                    },
                )]),
            },
        );
    }

    let ctx = ServerContext::with_sde_and_repository(Arc::new(sde), Arc::new(WorldRepository::with_demo_seed()));
    let char_id = 90_000_001_i64;
    let corp_id = 98_610_001_i64;
    let station_id = 60_003_760_i64;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.corporation_id = corp_id;
            ch.station_id = Some(station_id);
            ch.solar_system_id = 30_000_142;
            ch.clone_grade = 1; // Alpha clone -> exercises IndustryAlphaCloneTax
            ch.balance = IskCents::from_f64_isk(1_000_000_000.0);
        }
        snap.corporations.insert(
            corp_id,
            { let mut __corp = CorporationRecord::default();
                __corp.corporation_id = corp_id;
                __corp.name = "Cov Ind Corp".into();
                __corp.ticker = "CIND".into();
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
        for sid in [3380, 3387, 24625, 3406, 24624, 11448, 11452, 21791, 45746, 45748] {
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
fn queries_auth_params_remote_range_reactions_and_composite_reactor() {
    let (ctx, mut sess, char_id, corp_id, station_id) = build_ctx();
    let kw = HashMap::new();

    // Delegation to handle_industry_job_queries & unknown RPC None & unauthenticated errors
    assert!(handle_industry_jobs(&ctx, &mut sess, "industryManager", "GetJobCounts", &[], &kw).unwrap().is_ok());
    assert!(handle_industry_jobs(&ctx, &mut sess, "character", "GetCharactersToSelect", &[], &kw).is_none());

    let mut anon = SessionState::default();
    for m in ["InstallJob", "CancelJob", "CompleteJob"] {
        assert!(handle_industry_jobs(&ctx, &mut anon, "industryManager", m, &[], &kw).unwrap().is_err());
    }

    // Parameter validation errors: missing BP, invalid u8 activityID, runs > 1000
    assert_eq!(handle_industry_jobs(&ctx, &mut sess, "industryManager", "InstallJob", &[], &kw).unwrap().unwrap_err().code, "InvalidIndustryJobParameters");
    assert_eq!(handle_industry_jobs(&ctx, &mut sess, "industryManager", "InstallJob", &[PyRep::Integer(688), PyRep::Integer(500), PyRep::Integer(1)], &kw).unwrap().unwrap_err().code, "InvalidIndustryJobParameters");
    assert_eq!(handle_industry_jobs(&ctx, &mut sess, "industryManager", "InstallJob", &[PyRep::Integer(688), PyRep::Integer(1), PyRep::Integer(1001)], &kw).unwrap().unwrap_err().code, "InvalidIndustryJobParameters");

    // Remote range: FacilityNotFound, FacilityOutOfRemoteRange, and same-system remote station allowed
    assert_eq!(
        handle_industry_jobs(&ctx, &mut sess, "industryManager", "InstallJob", &[PyRep::Integer(10), PyRep::Integer(688), PyRep::Integer(1), PyRep::Integer(1), PyRep::Integer(69_999_999)], &kw).unwrap().unwrap_err().code,
        "FacilityNotFound"
    );
    let remote_st = ctx.sde.stations.values().filter(|s| s.solar_system_id != 30_000_142).min_by_key(|s| s.station_id).unwrap().station_id;
    assert_eq!(
        handle_industry_jobs(&ctx, &mut sess, "industryManager", "InstallJob", &[PyRep::Integer(10), PyRep::Integer(688), PyRep::Integer(3), PyRep::Integer(1), PyRep::Integer(remote_st)], &kw).unwrap().unwrap_err().code,
        "FacilityOutOfRemoteRange"
    );

    let same_sys_st = ctx
        .sde
        .stations
        .values()
        .filter(|s| s.solar_system_id == 30_000_142 && s.station_id != station_id)
        .min_by_key(|s| s.station_id)
        .unwrap()
        .station_id;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(71_001, mk_item(71_001, 688, char_id, same_sys_st, InventoryFlag::Hangar, 1, true));
    });
    // Same-system station passes remote jump range check with 0 Scientific Networking skill and installs TE research
    let te_rep = handle_industry_jobs(
        &ctx,
        &mut sess,
        "industryManager",
        "InstallJob",
        &[PyRep::Integer(71_001), PyRep::Integer(0), PyRep::Integer(3), PyRep::Integer(2), PyRep::Integer(same_sys_st)],
        &kw,
    )
    .unwrap()
    .unwrap();
    let te_jid = te_rep.dict_get("jobID").unwrap().as_i64().unwrap();
    ctx.db.with_mut_snapshot(|snap| snap.industry_jobs.get_mut(&te_jid).unwrap().start_unix = 0);
    handle_industry_jobs(&ctx, &mut sess, "industryManager", "CompleteJob", &[PyRep::Integer(te_jid)], &kw).unwrap().unwrap();
    ctx.db.with_snapshot(|snap| assert_eq!(snap.items[&71_001].dynamic_attributes.get(&ATTR_BLUEPRINT_TE), Some(&4.0)));

    // Reactions: NPC station, High-sec Athanor, Offline service, CompositeReactorRequired, MissingReactionSkills, BlueprintNotReactionFormula, InsufficientReactionMaterials, and valid Composite + Biochem reactions
    assert_eq!(handle_industry_jobs(&ctx, &mut sess, "industryManager", "InstallJob", &[PyRep::Integer(46_166), PyRep::Integer(9), PyRep::Integer(1)], &kw).unwrap().unwrap_err().code, "FacilityDoesNotSupportReactions");

    let lowsec_sys = ctx.sde.solar_systems.values().find(|s| s.solar_system_id < 30_100_000 && s.security_status < 0.40).unwrap().solar_system_id;
    let hs_ath = 1_046_000_001_i64;
    let ls_ath = 1_046_000_002_i64;
    ctx.db.with_mut_snapshot(|snap| {
        for (sid, sys, fuel) in [(hs_ath, 30_000_142, 100), (ls_ath, lowsec_sys, 0)] {
            snap.structures.insert(sid, UpwellStructureRecord {
                structure_id: sid, type_id: 35_835, owner_corp_id: corp_id, solar_system_id: sys,
                name: "Athanor".into(), state: "ShieldVulnerable".into(), quantum_core_installed: true, fuel_blocks: fuel, ..Default::default()
            });
        }
        snap.items.insert(71_010, mk_item(71_010, 90_274, char_id, ls_ath, InventoryFlag::Hangar, 1, true));
        snap.items.insert(71_011, mk_item(71_011, 46_166, char_id, ls_ath, InventoryFlag::Hangar, 1, true));
        snap.items.insert(71_012, mk_item(71_012, 46_167, char_id, ls_ath, InventoryFlag::Hangar, 1, true));
    });

    sess.station_id = None;
    sess.structure_id = Some(hs_ath);
    assert_eq!(handle_industry_jobs(&ctx, &mut sess, "industryManager", "InstallJob", &[PyRep::Integer(46_166), PyRep::Integer(9), PyRep::Integer(1)], &kw).unwrap().unwrap_err().code, "ReactionsForbiddenInHighSec");

    sess.structure_id = Some(ls_ath);
    sess.solar_system_id = Some(lowsec_sys);
    assert_eq!(handle_industry_jobs(&ctx, &mut sess, "industryManager", "InstallJob", &[PyRep::Integer(46_166), PyRep::Integer(9), PyRep::Integer(1)], &kw).unwrap().unwrap_err().code, "FacilityServiceOffline");

    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.get_mut(&ls_ath).unwrap().fuel_blocks = 500;
        snap.items.insert(71_020, mk_item(71_020, 45_539, corp_id, ls_ath, InventoryFlag::ServiceSlot0, 1, true));
        snap.module_online_states.insert(71_020, true);
    });
    assert_eq!(handle_industry_jobs(&ctx, &mut sess, "industryManager", "InstallJob", &[PyRep::Integer(90_274), PyRep::Integer(9), PyRep::Integer(1)], &kw).unwrap().unwrap_err().code, "CompositeReactorRequired");

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(71_021, mk_item(71_021, 45_537, corp_id, ls_ath, InventoryFlag::ServiceSlot1, 1, true));
        snap.module_online_states.insert(71_021, true);
        snap.skills.get_mut(&char_id).unwrap().remove(&45746);
    });
    assert_eq!(handle_industry_jobs(&ctx, &mut sess, "industryManager", "InstallJob", &[PyRep::Integer(46_166), PyRep::Integer(9), PyRep::Integer(1)], &kw).unwrap().unwrap_err().code, "MissingReactionSkills");

    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.get_mut(&char_id).unwrap().insert(45746, TrainedSkillRecord { type_id: 45746, level: 4, skill_points: 100_000.0 });
    });
    assert_eq!(handle_industry_jobs(&ctx, &mut sess, "industryManager", "InstallJob", &[PyRep::Integer(46_167), PyRep::Integer(9), PyRep::Integer(1)], &kw).unwrap().unwrap_err().code, "BlueprintNotReactionFormula");
    assert_eq!(handle_industry_jobs(&ctx, &mut sess, "industryManager", "InstallJob", &[PyRep::Integer(90_274), PyRep::Integer(9), PyRep::Integer(1)], &kw).unwrap().unwrap_err().code, "InsufficientReactionMaterials");

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(71_030, mk_item(71_030, 34, char_id, ls_ath, InventoryFlag::Hangar, 500, false));
    });
    let r_ok = handle_industry_jobs(
        &ctx,
        &mut sess,
        "industryManager",
        "InstallJob",
        &[PyRep::Dict(vec![
            (PyRep::String("blueprintTypeID".into()), PyRep::Integer(90_274)),
            (PyRep::String("activityID".into()), PyRep::Integer(9)),
            (PyRep::String("runs".into()), PyRep::Integer(2)),
        ])],
        &kw,
    )
    .unwrap()
    .unwrap();
    let r_jid = r_ok.dict_get("jobID").unwrap().as_i64().unwrap();
    ctx.db.with_mut_snapshot(|snap| snap.industry_jobs.get_mut(&r_jid).unwrap().start_unix = 0);
    handle_industry_jobs(&ctx, &mut sess, "industryManager", "CompleteJob", &[PyRep::Integer(r_jid)], &kw).unwrap().unwrap();

    // Restore station session for check completeness
    sess.structure_id = None;
    sess.station_id = Some(station_id);
}

#[test]
fn invention_relics_decryptors_failed_rolls_and_bpc_depletion() {
    let (ctx, mut sess, char_id, _corp_id, station_id) = build_ctx();
    let kw = HashMap::new();

    ctx.db.with_mut_snapshot(|snap| {
        let mut bpc1 = mk_item(72_001, 688, char_id, station_id, InventoryFlag::Hangar, 1, true);
        bpc1.is_blueprint_copy = true;
        bpc1.blueprint_runs_remaining = Some(1);
        snap.items.insert(72_001, bpc1);

        let mut bpc_bad = mk_item(72_002, 689, char_id, station_id, InventoryFlag::Hangar, 1, true);
        bpc_bad.is_blueprint_copy = true;
        bpc_bad.blueprint_runs_remaining = Some(2);
        snap.items.insert(72_002, bpc_bad);

        snap.items.insert(72_003, mk_item(72_003, 30_752, char_id, station_id, InventoryFlag::Hangar, 2, false));
    });

    // BPC errors: CannotResearchOrCopyBPC, InsufficientBlueprintRuns, BlueprintNotInventable, MissingInventionSkills, InsufficientInventionDatacores, InsufficientDecryptors
    assert_eq!(handle_industry_jobs(&ctx, &mut sess, "industryManager", "InstallJob", &[PyRep::Integer(688), PyRep::Integer(4), PyRep::Integer(1)], &kw).unwrap().unwrap_err().code, "CannotResearchOrCopyBPC");
    assert_eq!(handle_industry_jobs(&ctx, &mut sess, "industryManager", "InstallJob", &[PyRep::Integer(688), PyRep::Integer(1), PyRep::Integer(3)], &kw).unwrap().unwrap_err().code, "InsufficientBlueprintRuns");
    assert_eq!(handle_industry_jobs(&ctx, &mut sess, "industryManager", "InstallJob", &[PyRep::Integer(689), PyRep::Integer(8), PyRep::Integer(1)], &kw).unwrap().unwrap_err().code, "BlueprintNotInventable");

    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.get_mut(&char_id).unwrap().remove(&21791);
    });
    assert_eq!(handle_industry_jobs(&ctx, &mut sess, "industryManager", "InstallJob", &[PyRep::Integer(688), PyRep::Integer(8), PyRep::Integer(1)], &kw).unwrap().unwrap_err().code, "MissingInventionSkills");

    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.get_mut(&char_id).unwrap().insert(21791, TrainedSkillRecord { type_id: 21791, level: 5, skill_points: 256_000.0 });
    });
    assert_eq!(handle_industry_jobs(&ctx, &mut sess, "industryManager", "InstallJob", &[PyRep::Integer(688), PyRep::Integer(8), PyRep::Integer(1)], &kw).unwrap().unwrap_err().code, "InsufficientInventionDatacores");

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(72_010, mk_item(72_010, 20_424, char_id, station_id, InventoryFlag::Hangar, 5, false));
        snap.items.insert(72_011, mk_item(72_011, 20_172, char_id, station_id, InventoryFlag::Hangar, 5, false));
    });
    let mut kw_dec = HashMap::new();
    kw_dec.insert("decryptorID".into(), PyRep::Integer(34_201));
    assert_eq!(handle_industry_jobs(&ctx, &mut sess, "industryManager", "InstallJob", &[PyRep::Integer(688), PyRep::Integer(8), PyRep::Integer(1)], &kw_dec).unwrap().unwrap_err().code, "InsufficientDecryptors");

    // Ancient relic (72_003, qty 2) + decryptor succeeds and decrements relic qty to 1 on CompleteJob
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(72_012, mk_item(72_012, 34, char_id, station_id, InventoryFlag::Hangar, 500, false));
        snap.items.insert(72_013, mk_item(72_013, 34_201, char_id, station_id, InventoryFlag::Hangar, 2, false));
    });
    let relic_rep = handle_industry_jobs(&ctx, &mut sess, "industryManager", "InstallJob", &[PyRep::Integer(30_752), PyRep::Integer(8), PyRep::Integer(1)], &kw_dec).unwrap().unwrap();
    let relic_jid = relic_rep.dict_get("jobID").unwrap().as_i64().unwrap();
    ctx.db.with_mut_snapshot(|snap| snap.industry_jobs.get_mut(&relic_jid).unwrap().start_unix = 0);
    handle_industry_jobs(&ctx, &mut sess, "industryManager", "CompleteJob", &[PyRep::Integer(relic_jid)], &kw).unwrap().unwrap();
    ctx.db.with_snapshot(|snap| assert_eq!(snap.items[&72_003].quantity, 1));

    // Invention job with inv_prob forced very tiny (1e-9) -> fails invention roll and removes 1-run BPC 72_001
    let inv_rep = handle_industry_jobs(&ctx, &mut sess, "industryManager", "InstallJob", &[PyRep::Integer(688), PyRep::Integer(8), PyRep::Integer(1)], &kw).unwrap().unwrap();
    let inv_jid = inv_rep.dict_get("jobID").unwrap().as_i64().unwrap();
    ctx.db.with_mut_snapshot(|snap| {
        snap.industry_jobs.get_mut(&inv_jid).unwrap().start_unix = 0;
        let mut m = load_job_cost_meta(snap, inv_jid).unwrap();
        m.inv_prob = 1e-12;
        save_job_cost_meta(snap, inv_jid, &m);
    });
    let inv_fail_err = handle_industry_jobs(&ctx, &mut sess, "industryManager", "CompleteJob", &[PyRep::Integer(inv_jid)], &kw).unwrap().unwrap_err();
    assert_eq!(inv_fail_err.code, "InventionAttemptFailed");
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.items.contains_key(&72_001));
        assert_eq!(snap.industry_jobs[&inv_jid].status, "Failed");
    });

    // 1-run manufacturing BPC consumed and removed on CompleteJob + multi-stack material split/remove
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.retain(|_, i| i.type_id != 34);
        snap.items.insert(72_020, mk_item(72_020, 34, char_id, station_id, InventoryFlag::Hangar, 70, false));
        snap.items.insert(72_021, mk_item(72_021, 34, char_id, station_id, InventoryFlag::Hangar, 50, false));
        let mut mfg_bpc = mk_item(72_022, 688, char_id, station_id, InventoryFlag::Hangar, 1, true);
        mfg_bpc.is_blueprint_copy = true;
        mfg_bpc.blueprint_runs_remaining = Some(1);
        snap.items.insert(72_022, mfg_bpc);
    });
    let mfg_rep = handle_industry_jobs(&ctx, &mut sess, "industryManager", "InstallJob", &[PyRep::Integer(688), PyRep::Integer(1), PyRep::Integer(1)], &kw).unwrap().unwrap();
    let mfg_jid = mfg_rep.dict_get("jobID").unwrap().as_i64().unwrap();
    ctx.db.with_mut_snapshot(|snap| {
        assert!(!snap.items.contains_key(&72_020)); // First stack completely removed!
        assert_eq!(snap.items[&72_021].quantity, 20);
        snap.industry_jobs.get_mut(&mfg_jid).unwrap().start_unix = 0;
    });
    handle_industry_jobs(&ctx, &mut sess, "industryManager", "CompleteManyJobs", &[PyRep::Tuple(vec![PyRep::Integer(mfg_jid)])], &kw).unwrap().unwrap();
    ctx.db.with_snapshot(|snap| assert!(!snap.items.contains_key(&72_022)));
}

#[test]
fn corp_roles_funds_containers_cancel_refund_and_complete_guards() {
    let (ctx, mut sess, char_id, corp_id, station_id) = build_ctx();
    let other_char = 90_000_002_i64;
    let kw = HashMap::new();

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch2) = snap.characters.get_mut(&other_char) {
            ch2.corporation_id = corp_id;
        }
        snap.items.insert(73_001, mk_item(73_001, 688, char_id, station_id, InventoryFlag::Hangar, 1, true));
        snap.items.insert(73_002, mk_item(73_002, 688, corp_id, station_id, InventoryFlag::CorpSAG2, 1, true));
        snap.items.insert(73_003, mk_item(73_003, 34, corp_id, station_id, InventoryFlag::CorpSAG2, 100, false));
        snap.items.insert(73_004, mk_item(73_004, 34, char_id, station_id, InventoryFlag::Hangar, 1_000, false));
        // Wrong-location container & wrong-owner container
        snap.items.insert(73_010, mk_item(73_010, 3293, char_id, 60_003_761, InventoryFlag::Hangar, 1, true));
        snap.items.insert(73_011, mk_item(73_011, 3293, other_char, station_id, InventoryFlag::Hangar, 1, true));
    });

    // OwnerAccessDenied on wrong ownerID
    assert_eq!(
        handle_industry_jobs(&ctx, &mut sess, "industryManager", "InstallJob", &[PyRep::keyval([("blueprintTypeID", PyRep::Integer(688)), ("ownerID", PyRep::Integer(98_999_999))])], &kw).unwrap().unwrap_err().code,
        "OwnerAccessDenied"
    );

    // Container validation errors: missing & wrong-location input container; missing & wrong-owner output location
    for (k, v, code) in [
        ("inputContainerID", 999_999_i64, "InvalidInputContainer"),
        ("inputContainerID", 73_010, "InvalidInputContainer"),
        ("outputLocationID", 999_999, "InvalidOutputLocation"),
        ("outputLocationID", 73_011, "InvalidOutputLocation"),
    ] {
        assert_eq!(
            handle_industry_jobs(&ctx, &mut sess, "industryManager", "InstallJob", &[PyRep::keyval([("blueprintItemID", PyRep::Integer(73_001)), ("blueprintTypeID", PyRep::Integer(688)), (k, PyRep::Integer(v))])], &kw).unwrap().unwrap_err().code,
            code
        );
    }

    // Character InsufficientFunds
    ctx.db.with_mut_snapshot(|snap| snap.characters.get_mut(&char_id).unwrap().balance = IskCents(0));
    assert_eq!(handle_industry_jobs(&ctx, &mut sess, "industryManager", "InstallJob", &[PyRep::Integer(688), PyRep::Integer(1), PyRep::Integer(1)], &kw).unwrap().unwrap_err().code, "InsufficientFunds");
    ctx.db.with_mut_snapshot(|snap| snap.characters.get_mut(&char_id).unwrap().balance = IskCents::from_f64_isk(1_000_000_000.0));

    // Corp job: missing Factory Manager -> CrpAccessDenied; missing division 2 Take -> CrpAccessDenied; corp InsufficientFunds -> InsufficientFunds
    assert_eq!(
        handle_industry_jobs(&ctx, &mut sess, "industryManager", "InstallJob", &[PyRep::keyval([("blueprintItemID", PyRep::Integer(73_002)), ("blueprintTypeID", PyRep::Integer(688)), ("useCorp", PyRep::Bool(true))])], &kw).unwrap().unwrap_err().code,
        "CrpAccessDenied"
    );
    sess.corp_role = corp_roles::ROLE_FACTORY_MANAGER;
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.get_mut(&corp_id).unwrap().member_roles.insert(char_id, corp_roles::ROLE_FACTORY_MANAGER | corp_roles::ROLE_ACCOUNT_CAN_TAKE[0]);
    });
    assert_eq!(
        handle_industry_jobs(&ctx, &mut sess, "industryManager", "InstallJob", &[PyRep::keyval([("blueprintItemID", PyRep::Integer(73_002)), ("blueprintTypeID", PyRep::Integer(688)), ("useCorp", PyRep::Bool(true)), ("inputFlag", PyRep::Integer(InventoryFlag::CorpSAG2 as i64))])], &kw).unwrap().unwrap_err().code,
        "CrpAccessDenied"
    );

    sess.corp_role = corp_roles::ROLE_FACTORY_MANAGER | (1u64 << 14) | corp_roles::ROLE_ACCOUNT_CAN_TAKE[0]; // Division 2 Take + Account Take 1
    ctx.db.with_mut_snapshot(|snap| {
        let c = snap.corporations.get_mut(&corp_id).unwrap();
        c.member_roles.insert(char_id, corp_roles::ROLE_FACTORY_MANAGER | (1u64 << 14) | corp_roles::ROLE_ACCOUNT_CAN_TAKE[0]);
        c.set_division_balance(1, IskCents(0));
    });
    assert_eq!(
        handle_industry_jobs(&ctx, &mut sess, "industryManager", "InstallJob", &[PyRep::keyval([("blueprintItemID", PyRep::Integer(73_002)), ("blueprintTypeID", PyRep::Integer(688)), ("useCorp", PyRep::Bool(true)), ("inputFlag", PyRep::Integer(InventoryFlag::CorpSAG2 as i64))])], &kw).unwrap().unwrap_err().code,
        "InsufficientFunds"
    );

    // Restore corp wallet -> install corp job consuming exact 100x Tritanium (stack 73_003 removed!)
    ctx.db.with_mut_snapshot(|snap| {
        let c = snap.corporations.get_mut(&corp_id).unwrap();
        c.set_division_balance(1, IskCents::from_f64_isk(1_000_000_000.0));
        c.set_division_balance(1, IskCents::from_f64_isk(1_000_000_000.0));
    });
    let cj_rep = handle_industry_jobs(
        &ctx,
        &mut sess,
        "industryManager",
        "InstallJob",
        &[PyRep::keyval([
            ("blueprintItemID", PyRep::Integer(73_002)),
            ("blueprintTypeID", PyRep::Integer(688)),
            ("ownerID", PyRep::Integer(corp_id)),
            ("inputFlag", PyRep::Integer(InventoryFlag::CorpSAG2 as i64)),
        ])],
        &kw,
    )
    .unwrap()
    .unwrap();
    let cj_id = cj_rep.dict_get("jobID").unwrap().as_i64().unwrap();

    // Ex-member cannot cancel or complete corp job; colleague without role cannot cancel; colleague WITH Director CAN cancel and creates fresh CorpSAG1 refund stack!
    let mut s_other = sess.clone();
    s_other.character_id = Some(other_char);
    s_other.corporation_id = Some(98_999_999);
    s_other.corp_role = 0;
    ctx.db.with_mut_snapshot(|snap| snap.characters.get_mut(&other_char).unwrap().corporation_id = 98_999_999);
    assert_eq!(handle_industry_jobs(&ctx, &mut s_other, "industryManager", "CancelJob", &[PyRep::Integer(cj_id)], &kw).unwrap().unwrap_err().code, "JobOwnershipViolation");
    assert_eq!(handle_industry_jobs(&ctx, &mut s_other, "industryManager", "CompleteJob", &[PyRep::Integer(cj_id)], &kw).unwrap().unwrap_err().code, "JobOwnershipViolation");

    ctx.db.with_mut_snapshot(|snap| snap.characters.get_mut(&other_char).unwrap().corporation_id = corp_id);
    s_other.corporation_id = Some(corp_id);
    assert_eq!(handle_industry_jobs(&ctx, &mut s_other, "industryManager", "CancelJob", &[PyRep::Integer(cj_id)], &kw).unwrap().unwrap_err().code, "JobOwnershipViolation");
    s_other.corp_role = corp_roles::ROLE_DIRECTOR;
    handle_industry_jobs(&ctx, &mut s_other, "industryManager", "CancelJob", &[PyRep::Integer(cj_id)], &kw).unwrap().unwrap();

    // CompleteJob error guards: empty args -> JobNotFound, Paused -> FacilityServiceOffline, Cancelled -> InvalidJobState, unlocked BP -> BlueprintMissingOrUnlocked, stranger -> JobOwnershipViolation
    assert_eq!(handle_industry_jobs(&ctx, &mut sess, "industryManager", "CompleteJob", &[], &kw).unwrap().unwrap_err().code, "JobNotFound");
    assert_eq!(handle_industry_jobs(&ctx, &mut sess, "industryManager", "CompleteJob", &[PyRep::Integer(cj_id)], &kw).unwrap().unwrap_err().code, "InvalidJobState");

    let pj_rep = handle_industry_jobs(&ctx, &mut sess, "industryManager", "InstallJob", &[PyRep::Integer(688), PyRep::Integer(1), PyRep::Integer(1)], &kw).unwrap().unwrap();
    let pj_id = pj_rep.dict_get("jobID").unwrap().as_i64().unwrap();
    assert_eq!(handle_industry_jobs(&ctx, &mut s_other, "industryManager", "CancelJob", &[PyRep::Integer(pj_id)], &kw).unwrap().unwrap_err().code, "JobOwnershipViolation");
    assert_eq!(handle_industry_jobs(&ctx, &mut s_other, "industryManager", "CompleteJob", &[PyRep::Integer(pj_id)], &kw).unwrap().unwrap_err().code, "JobOwnershipViolation");

    ctx.db.with_mut_snapshot(|snap| {
        let j = snap.industry_jobs.get_mut(&pj_id).unwrap();
        j.start_unix = 0;
        j.status = "Paused".into();
        // Force facility to non-existent Upwell structure so sync keeps Paused
        snap.structures.insert(9_999_001, UpwellStructureRecord { structure_id: 9_999_001, state: "Destroyed".into(), ..Default::default() });
        j.facility_id = 9_999_001;
    });
    assert_eq!(handle_industry_jobs(&ctx, &mut sess, "industryManager", "CompleteJob", &[PyRep::Integer(pj_id)], &kw).unwrap().unwrap_err().code, "FacilityServiceOffline");

    ctx.db.with_mut_snapshot(|snap| {
        let j = snap.industry_jobs.get_mut(&pj_id).unwrap();
        j.facility_id = station_id;
        j.status = "Active".into();
        snap.items.get_mut(&73_001).unwrap().flag = InventoryFlag::Hangar; // Unlock BP!
    });
    assert_eq!(handle_industry_jobs(&ctx, &mut sess, "industryManager", "CompleteJob", &[PyRep::Integer(pj_id)], &kw).unwrap().unwrap_err().code, "BlueprintMissingOrUnlocked");
}

#[test]
fn lifecycle_cancel_paused_ceo_refund_stacking_and_complete_edge_paths() {
    let (ctx, mut sess, char_id, corp_id, station_id) = build_ctx();
    let other_char = 90_000_002_i64;
    let kw = HashMap::new();

    // Non-lifecycle method returns None from handle_cancel_or_complete_industry_job
    assert!(super::industry_jobs_lifecycle::handle_cancel_or_complete_industry_job(
        &ctx,
        &mut sess,
        "industryManager",
        "GetJobCounts",
        &[]
    )
    .is_none());

    // CancelJob on missing job -> JobNotFound
    assert_eq!(
        handle_industry_jobs(&ctx, &mut sess, "industryManager", "CancelJob", &[PyRep::Integer(999_999)], &kw)
            .unwrap()
            .unwrap_err()
            .code,
        "JobNotFound"
    );

    // Seed corp member roles & CEO & items
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch2) = snap.characters.get_mut(&other_char) {
            ch2.corporation_id = corp_id;
        }
        if let Some(c) = snap.corporations.get_mut(&corp_id) {
            c.ceo_id = other_char;
        }
        snap.items.retain(|_, i| !(i.owner_id == char_id && i.type_id == 34 && i.location_id == station_id));
        snap.items.insert(74_001, mk_item(74_001, 688, char_id, station_id, InventoryFlag::Hangar, 1, true));
        snap.items.insert(74_002, mk_item(74_002, 34, char_id, station_id, InventoryFlag::Hangar, 200, false));
        // Corp BPO + materials + existing CorpSAG1 stack to verify refund accumulation
        snap.items.insert(74_003, mk_item(74_003, 688, corp_id, station_id, InventoryFlag::CorpSAG1, 1, true));
        snap.items.insert(74_004, mk_item(74_004, 34, corp_id, station_id, InventoryFlag::CorpSAG1, 150, false));
    });

    // Install personal job, then mark it Paused and cancel it -> allowed & refunds onto existing stack 74_002 + skips 0-qty entry
    let p_rep = handle_industry_jobs(
        &ctx,
        &mut sess,
        "industryManager",
        "InstallJob",
        &[PyRep::Integer(688), PyRep::Integer(1), PyRep::Integer(1)],
        &kw,
    )
    .unwrap()
    .unwrap();
    let p_jid = p_rep.dict_get("jobID").unwrap().as_i64().unwrap();
    ctx.db.with_mut_snapshot(|snap| {
        snap.industry_jobs.get_mut(&p_jid).unwrap().status = "Paused".into();
        let mut m = load_job_cost_meta(snap, p_jid).unwrap();
        m.consumed_materials.push((34, 0)); // skipped on refund
        save_job_cost_meta(snap, p_jid, &m);
    });
    let cancel_paused = handle_industry_jobs(&ctx, &mut sess, "industryManager", "CancelJob", &[PyRep::Integer(p_jid)], &kw)
        .unwrap()
        .unwrap();
    assert_eq!(cancel_paused.dict_get("status").and_then(|v| v.as_str()), Some("Cancelled"));
    ctx.db.with_snapshot(|snap| {
        let total_trit: i32 = snap
            .items
            .values()
            .filter(|i| i.owner_id == char_id && i.type_id == 34 && i.location_id == station_id)
            .map(|i| i.quantity)
            .sum();
        assert_eq!(total_trit, 200);
    });

    // Cancelling an already-Cancelled job -> InvalidJobState
    assert_eq!(
        handle_industry_jobs(&ctx, &mut sess, "industryManager", "CancelJob", &[PyRep::Integer(p_jid)], &kw)
            .unwrap()
            .unwrap_err()
            .code,
        "InvalidJobState"
    );

    // Corp job installed by char_id, cancelled by CEO (other_char) with session.corp_role == 0
    let corp_install_roles =
        corp_roles::ROLE_FACTORY_MANAGER | (1u64 << 13) | corp_roles::ROLE_ACCOUNT_CAN_TAKE[0];
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.corporations.get_mut(&corp_id) {
            c.member_roles.insert(char_id, corp_install_roles);
        }
    });
    sess.corp_role = corp_install_roles; // Factory Manager + CorpSAG1 take + Div 1 Account Take
    let c_rep = handle_industry_jobs(
        &ctx,
        &mut sess,
        "industryManager",
        "InstallJob",
        &[PyRep::keyval([
            ("blueprintItemID", PyRep::Integer(74_003)),
            ("blueprintTypeID", PyRep::Integer(688)),
            ("ownerID", PyRep::Integer(corp_id)),
            ("inputFlag", PyRep::Integer(InventoryFlag::CorpSAG1 as i64)),
        ])],
        &kw,
    )
    .unwrap()
    .unwrap();
    let c_jid = c_rep.dict_get("jobID").unwrap().as_i64().unwrap();
    let mut ceo_sess = sess.clone();
    ceo_sess.character_id = Some(other_char);
    ceo_sess.corp_role = 0; // relies on snap.corporations CEO check
    let ceo_cancel = handle_industry_jobs(&ctx, &mut ceo_sess, "industryManager", "CancelJob", &[PyRep::Integer(c_jid)], &kw)
        .unwrap()
        .unwrap();
    assert_eq!(ceo_cancel.dict_get("status").and_then(|v| v.as_str()), Some("Cancelled"));
    // Existing CorpSAG1 stack (50 remaining) received the 100 refunded back -> 150
    ctx.db.with_snapshot(|snap| assert_eq!(snap.items[&74_004].quantity, 150));

    // JobNotYetComplete when start_unix + duration_seconds > now_unix
    let early_rep = handle_industry_jobs(
        &ctx,
        &mut sess,
        "industryManager",
        "InstallJob",
        &[PyRep::Integer(688), PyRep::Integer(1), PyRep::Integer(1)],
        &kw,
    )
    .unwrap()
    .unwrap();
    let early_jid = early_rep.dict_get("jobID").unwrap().as_i64().unwrap();
    assert_eq!(
        handle_industry_jobs(&ctx, &mut sess, "industryManager", "CompleteJob", &[PyRep::Integer(early_jid)], &kw)
            .unwrap()
            .unwrap_err()
            .code,
        "JobNotYetComplete"
    );
    handle_industry_jobs(&ctx, &mut sess, "industryManager", "CancelJob", &[PyRep::Integer(early_jid)], &kw).unwrap().unwrap();

    // ME research (activity 4) increments ME capped at 10.0; TE research (activity 3) caps TE at 20.0;
    // Copy (activity 5) inherits parent ME/TE & handles licensed_runs: None -> 1;
    // CompleteManyJobs with empty list -> JobNotFound; with List of 2 jobs -> returns List of 2 results
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&74_001).unwrap().dynamic_attributes.insert(crate::services::industry_pi::ATTR_BLUEPRINT_ME, 8.0);
        snap.items.get_mut(&74_001).unwrap().dynamic_attributes.insert(ATTR_BLUEPRINT_TE, 18.0);
        snap.items.insert(74_005, mk_item(74_005, 688, char_id, station_id, InventoryFlag::Hangar, 1, true));
    });
    let me_rep = handle_industry_jobs(
        &ctx,
        &mut sess,
        "industryManager",
        "InstallJob",
        &[PyRep::keyval([
            ("blueprintItemID", PyRep::Integer(74_001)),
            ("blueprintTypeID", PyRep::Integer(688)),
            ("activityID", PyRep::Integer(4)),
            ("runs", PyRep::Integer(2)),
        ])],
        &kw,
    )
    .unwrap()
    .unwrap();
    let me_jid = me_rep.dict_get("jobID").unwrap().as_i64().unwrap();

    let te_rep = handle_industry_jobs(
        &ctx,
        &mut sess,
        "industryManager",
        "InstallJob",
        &[PyRep::keyval([
            ("blueprintItemID", PyRep::Integer(74_005)),
            ("blueprintTypeID", PyRep::Integer(688)),
            ("activityID", PyRep::Integer(3)),
            ("runs", PyRep::Integer(10)),
        ])],
        &kw,
    )
    .unwrap()
    .unwrap();
    let te_jid = te_rep.dict_get("jobID").unwrap().as_i64().unwrap();

    ctx.db.with_mut_snapshot(|snap| {
        snap.industry_jobs.get_mut(&me_jid).unwrap().start_unix = 0;
        snap.industry_jobs.get_mut(&te_jid).unwrap().start_unix = 0;
        // Set TE to 18.0 so runs=10 (+20) tests .min(20.0) clamp on CompleteJob
        snap.items
            .get_mut(&74_005)
            .unwrap()
            .dynamic_attributes
            .insert(ATTR_BLUEPRINT_TE, 18.0);
    });
    assert_eq!(
        handle_industry_jobs(&ctx, &mut sess, "industryManager", "CompleteManyJobs", &[PyRep::List(vec![])], &kw)
            .unwrap()
            .unwrap_err()
            .code,
        "JobNotFound"
    );
    let many_rep = handle_industry_jobs(
        &ctx,
        &mut sess,
        "industryManager",
        "CompleteManyJobs",
        &[PyRep::List(vec![PyRep::Integer(me_jid), PyRep::Integer(te_jid)])],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert!(matches!(many_rep, PyRep::List(ref l) if l.len() == 2));
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items[&74_001].dynamic_attributes.get(&crate::services::industry_pi::ATTR_BLUEPRINT_ME), Some(&10.0));
        assert_eq!(snap.items[&74_005].dynamic_attributes.get(&ATTR_BLUEPRINT_TE), Some(&20.0));
    });

    // Manufacturing on multi-run BPC inside a container decrements runs_remaining without deleting BPC
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(74_010, mk_item(74_010, 3293, char_id, station_id, InventoryFlag::Hangar, 1, true));
        let mut bpc = mk_item(74_011, 688, char_id, 74_010, InventoryFlag::Hangar, 1, true);
        bpc.is_blueprint_copy = true;
        bpc.blueprint_runs_remaining = Some(5);
        snap.items.insert(74_011, bpc);
        snap.items.insert(74_012, mk_item(74_012, 34, char_id, station_id, InventoryFlag::Hangar, 500, false));
    });
    let mfg_bpc_rep = handle_industry_jobs(
        &ctx,
        &mut sess,
        "industryManager",
        "InstallJob",
        &[PyRep::keyval([
            ("blueprintItemID", PyRep::Integer(74_011)),
            ("blueprintTypeID", PyRep::Integer(688)),
            ("activityID", PyRep::Integer(1)),
            ("runs", PyRep::Integer(2)),
        ])],
        &kw,
    )
    .unwrap()
    .unwrap();
    let mfg_bpc_jid = mfg_bpc_rep.dict_get("jobID").unwrap().as_i64().unwrap();
    ctx.db.with_mut_snapshot(|snap| snap.industry_jobs.get_mut(&mfg_bpc_jid).unwrap().start_unix = 0);
    handle_industry_jobs(&ctx, &mut sess, "industryManager", "CompleteJob", &[PyRep::Integer(mfg_bpc_jid)], &kw)
        .unwrap()
        .unwrap();
    ctx.db.with_snapshot(|snap| {
        let bpc = &snap.items[&74_011];
        assert_eq!(bpc.blueprint_runs_remaining, Some(3));
        assert_eq!(bpc.flag, InventoryFlag::Hangar);
    });

    // Invention fallback branches:
    // (a) multi-run BPC decrements runs instead of removing
    // (b) meta.inv_prob == 0.0 computes probability from skills/SDE
    // (c) job.product_type_id == 0 -> fallback 11372 and meta.invented_runs == 0 -> ME=2, TE=4, runs=1
    // (d) ancient relic with quantity == 1 is removed on completion
    ctx.db.with_mut_snapshot(|snap| {
        let mut inv_bpc = mk_item(74_020, 688, char_id, station_id, InventoryFlag::Locked, 1, true);
        inv_bpc.is_blueprint_copy = true;
        inv_bpc.blueprint_runs_remaining = Some(4);
        snap.items.insert(74_020, inv_bpc);
        let job_id_inv = 980_001_i64;
        snap.industry_jobs.insert(
            job_id_inv,
            eve_db::IndustryJobRecord {
                job_id: job_id_inv,
                installer_id: char_id,
                facility_id: station_id,
                blueprint_item_id: 74_020,
                blueprint_type_id: 688,
                blueprint_location_id: station_id,
                activity_id: 8,
                product_type_id: 0, // triggers 11372 fallback!
                runs: 1,
                licensed_runs: None,
                status: "Active".into(),
                start_unix: 0,
                duration_seconds: 10,
                cost: Some(100.0),
                ..Default::default()
            },
        );
        // Save zeroed JobCostMeta so inv_prob == 0.0 and invented_runs == 0
        save_job_cost_meta(snap, job_id_inv, &Default::default());

        // Single-qty ancient relic (qty == runs == 1) -> removed on CompleteJob
        snap.items.insert(74_021, mk_item(74_021, 30_752, char_id, station_id, InventoryFlag::Locked, 1, false));
        let relic_jid = 980_002_i64;
        snap.industry_jobs.insert(
            relic_jid,
            eve_db::IndustryJobRecord {
                job_id: relic_jid,
                installer_id: char_id,
                facility_id: station_id,
                blueprint_item_id: 74_021,
                blueprint_type_id: 30_752,
                blueprint_location_id: station_id,
                activity_id: 8,
                product_type_id: 29_984,
                runs: 1,
                licensed_runs: None,
                status: "Active".into(),
                start_unix: 0,
                duration_seconds: 10,
                cost: Some(100.0),
                ..Default::default()
            },
        );
        save_job_cost_meta(snap, relic_jid, &Default::default());

        // Copy job with licensed_runs: None -> defaults to 1 copy run and inherits ME/TE
        let copy_jid = 980_003_i64;
        snap.items.get_mut(&74_001).unwrap().flag = InventoryFlag::Locked;
        snap.industry_jobs.insert(
            copy_jid,
            eve_db::IndustryJobRecord {
                job_id: copy_jid,
                installer_id: char_id,
                facility_id: station_id,
                blueprint_item_id: 74_001,
                blueprint_type_id: 688,
                blueprint_location_id: station_id,
                activity_id: 5,
                product_type_id: 688,
                runs: 1,
                licensed_runs: None,
                status: "Active".into(),
                start_unix: 0,
                duration_seconds: 10,
                cost: Some(100.0),
                ..Default::default()
            },
        );
        save_job_cost_meta(snap, copy_jid, &Default::default());
    });

    let inv_done = handle_industry_jobs(&ctx, &mut sess, "industryManager", "CompleteJob", &[PyRep::Integer(980_001)], &kw)
        .unwrap()
        .unwrap();
    assert_eq!(inv_done.dict_get("productTypeID"), Some(&PyRep::Integer(11_372)));
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items[&74_020].blueprint_runs_remaining, Some(3));
        let prod_id = inv_done.dict_get("deliveredItemID").unwrap().as_i64().unwrap();
        let prod = &snap.items[&prod_id];
        assert_eq!(prod.blueprint_runs_remaining, Some(1));
        assert_eq!(prod.dynamic_attributes.get(&crate::services::industry_pi::ATTR_BLUEPRINT_ME), Some(&2.0));
        assert_eq!(prod.dynamic_attributes.get(&ATTR_BLUEPRINT_TE), Some(&4.0));
    });

    handle_industry_jobs(&ctx, &mut sess, "industryManager", "CompleteJob", &[PyRep::Integer(980_002)], &kw)
        .unwrap()
        .unwrap();
    ctx.db.with_snapshot(|snap| assert!(!snap.items.contains_key(&74_021)));

    let copy_done = handle_industry_jobs(&ctx, &mut sess, "industryManager", "CompleteJob", &[PyRep::Integer(980_003)], &kw)
        .unwrap()
        .unwrap();
    let copy_pid = copy_done.dict_get("deliveredItemID").unwrap().as_i64().unwrap();
    ctx.db.with_snapshot(|snap| {
        let copy = &snap.items[&copy_pid];
        assert!(copy.is_blueprint_copy);
        assert_eq!(copy.blueprint_runs_remaining, Some(1));
        assert_eq!(copy.dynamic_attributes.get(&crate::services::industry_pi::ATTR_BLUEPRINT_ME), Some(&10.0));
    });

    // #956 & #982:
    // - blueprintManager.GetBlueprintData & GetBlueprintDataByOwner always emit integer solarSystemID, copy, jobID
    // - industryManager.InstallJob emits OnAccountChange
    // - /completeindustry emits OnIndustryJob with status = 3 (STATUS_READY)
    // - industryManager.CompleteJob delivers 1-run manufactured item with singleton == false and emits OnItemChange
    let bp_data_svc = crate::services::industry_pi::handle_industry_pi(
        &ctx,
        &mut sess,
        "blueprintManager",
        "GetBlueprintData",
        &[PyRep::Integer(74_001)],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(bp_data_svc.dict_get("solarSystemID"), Some(&PyRep::Integer(30_000_142)));
    assert_eq!(bp_data_svc.dict_get("copy"), Some(&PyRep::Bool(false)));

    let bp_owner_svc = crate::services::industry_pi::handle_industry_pi(
        &ctx,
        &mut sess,
        "blueprintManager",
        "GetBlueprintDataByOwner",
        &[PyRep::Integer(char_id)],
        &kw,
    )
    .unwrap()
    .unwrap();
    let PyRep::List(ref owner_rows) = bp_owner_svc else {
        panic!("expected List from service-level GetBlueprintDataByOwner");
    };
    assert!(owner_rows.iter().all(|r| r.dict_get("solarSystemID") == Some(&PyRep::Integer(30_000_142))));

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(74_030, mk_item(74_030, 688, char_id, station_id, InventoryFlag::Hangar, 1, true));
        snap.items.insert(74_031, mk_item(74_031, 34, char_id, station_id, InventoryFlag::Hangar, 50_000, false));
    });
    let _ = ctx.drain_notifications(char_id);
    let mfg_1run = handle_industry_jobs(
        &ctx,
        &mut sess,
        "industryManager",
        "InstallJob",
        &[PyRep::keyval([
            ("blueprintItemID", PyRep::Integer(74_030)),
            ("blueprintTypeID", PyRep::Integer(688)),
            ("activityID", PyRep::Integer(1)),
            ("runs", PyRep::Integer(1)),
        ])],
        &kw,
    )
    .unwrap()
    .unwrap();
    let mfg_1run_jid = mfg_1run.dict_get("jobID").unwrap().as_i64().unwrap();
    let install_notifs = ctx.drain_notifications(char_id);
    assert!(
        install_notifs.iter().any(|n| n.method == "OnAccountChange"),
        "InstallJob must emit OnAccountChange for installation fee debit (#982)"
    );

    crate::services::notify_emitters_character_services::handle_batch3_gm_slash_cmd(
        &ctx,
        char_id,
        Some(30_000_142),
        &format!("/completeindustry {mfg_1run_jid}"),
    )
    .unwrap()
    .unwrap();
    let ready_notifs = ctx.drain_notifications(char_id);
    assert!(
        ready_notifs.iter().any(|n| {
            n.method == "OnIndustryJob"
                && n.args.first() == Some(&PyRep::Integer(mfg_1run_jid))
                && n.args.get(4) == Some(&PyRep::Integer(3))
        }),
        "/completeindustry must emit OnIndustryJob with status = 3 (STATUS_READY) (#982)"
    );

    let comp_1run = handle_industry_jobs(
        &ctx,
        &mut sess,
        "industryManager",
        "CompleteJob",
        &[PyRep::Integer(mfg_1run_jid)],
        &kw,
    )
    .unwrap()
    .unwrap();
    let delivered_1run_id = comp_1run.dict_get("deliveredItemID").unwrap().as_i64().unwrap();
    ctx.db.with_snapshot(|snap| {
        let delivered = &snap.items[&delivered_1run_id];
        assert!(
            !delivered.singleton,
            "1-run manufactured item must be delivered repackaged (singleton == false) (#982)"
        );
    });
    let comp_notifs = ctx.drain_notifications(char_id);
    let item_change_count = comp_notifs.iter().filter(|n| n.method == "OnItemChange").count();
    assert!(
        item_change_count >= 2,
        "CompleteJob must emit OnItemChange for unlocked blueprint and delivered item (#982), got {item_change_count}"
    );
}
