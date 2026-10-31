//! Extended Industry, Planetary Interaction, and Wallet/Item Trader parity tests.

use crate::common;
use eve_db::{
    InventoryFlag, InventoryItemRecord, IskCents, TrainedSkillRecord, TypeId, UpwellStructureRecord,
};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use std::collections::HashMap;

const JITA_SOLAR_SYSTEM_ID: u32 = 30_000_142;
const JITA_STATION_ID: i64 = 60_003_760;
const LOWSEC_SOLAR_SYSTEM_ID: u32 = 30_000_012;
const LOWSEC_STATION_ID: i64 = 60_012_292;
const NULLSEC_SOLAR_SYSTEM_ID: u32 = 30_004_759;

const RIFTER_BP_TYPE_ID: TypeId = 691;

async fn setup_pilot(
    ctx: &ServerContext,
    session_id: u64,
    username: &str,
    password: &str,
    char_id: i64,
) -> SessionState {
    let kw = HashMap::new();
    let mut sess = SessionState::new(session_id);
    common::login_pilot(ctx, &mut sess, username, password).await;
    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID");
    sess
}

fn move_pilot(
    ctx: &ServerContext,
    sess: &mut SessionState,
    char_id: i64,
    sys_id: u32,
    station: Option<i64>,
    structure: Option<i64>,
) {
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = sys_id;
            ch.station_id = station;
            ch.structure_id = structure;
        }
    });
    sess.solar_system_id = Some(sys_id);
    sess.station_id = station;
    sess.structure_id = structure;
}

fn make_structure(
    sid: i64,
    stype: TypeId,
    corp_id: i64,
    ssys: u32,
    name: &str,
) -> UpwellStructureRecord {
    UpwellStructureRecord {
        structure_id: sid,
        type_id: stype,
        owner_corp_id: corp_id,
        solar_system_id: ssys,
        name: name.to_string(),
        position: [100_000.0, 0.0, 0.0],
        state: "ShieldVulnerable".into(),
        quantum_core_installed: true,
        fuel_blocks: 5_000,
        shield_hp: 1.0,
        armor_hp: 1.0,
        hull_hp: 1.0,
        reinforced_until_unix: 0,
        last_damage_tick: 0,
        last_damage_unix: 0, self_destruct_at_unix: None,
    }
}

fn seed_item(
    snap: &mut eve_db::WorldStateSnapshot,
    type_id: TypeId,
    owner_id: i64,
    location_id: i64,
    flag: InventoryFlag,
    quantity: i32,
    singleton: bool,
    is_blueprint_copy: bool,
    runs: Option<i32>,
) -> i64 {
    snap.next_entity_id += 1;
    let item_id = snap.next_entity_id;
    let mut dynamic_attributes = HashMap::new();
    if is_blueprint_copy {
        dynamic_attributes.insert(1115, 1.0);
    }
    if let Some(r) = runs {
        dynamic_attributes.insert(1124, r as f64);
    }
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
            version: 1,
            dynamic_attributes,
            source_mutaplasmid_id: None,
            insured_payout_isk: 0.0,
            is_blueprint_copy,
            blueprint_runs_remaining: runs,
        },
    );
    item_id
}

#[tokio::test]
async fn test_industry_blueprint_copying_me_te_and_repackage() {
    // parity: feat:industry.blueprint-copying-runs, feat:industry.blueprint-me-research, feat:industry.blueprint-te-research, feat:industry.blueprint-pack-unpack
    eve_testkit::attest(&[
        "feat:industry.blueprint-copying-runs",
        "feat:industry.blueprint-me-research",
        "feat:industry.blueprint-te-research",
        "feat:industry.blueprint-pack-unpack",
    ]);
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let kw = HashMap::new();
    let char_id = 90_000_001;
    let mut sess = setup_pilot(&ctx, 701, "demo", "demo-secret", char_id).await;

    let (bpo_copy_1, bpo_copy_2, bpo_me, bpo_te, bpo_pack) = ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.balance = IskCents::from_isk(500_000_000);
        }
        let sk = snap.skills.entry(char_id).or_default();
        for (stid, lvl) in [(3380, 5), (3387, 5), (3406, 5), (24624, 5)] {
            sk.insert(
                stid,
                TrainedSkillRecord {
                    type_id: stid,
                    level: lvl,
                    skill_points: 256_000.0,
                },
            );
        }
        for stid in [3402, 3403, 3409, 3388] {
            sk.remove(&stid);
        }
        (
            seed_item(snap, RIFTER_BP_TYPE_ID, char_id, JITA_STATION_ID, InventoryFlag::Hangar, 1, true, false, None),
            seed_item(snap, RIFTER_BP_TYPE_ID, char_id, JITA_STATION_ID, InventoryFlag::Hangar, 1, true, false, None),
            seed_item(snap, RIFTER_BP_TYPE_ID, char_id, JITA_STATION_ID, InventoryFlag::Hangar, 1, true, false, None),
            seed_item(snap, RIFTER_BP_TYPE_ID, char_id, JITA_STATION_ID, InventoryFlag::Hangar, 1, true, false, None),
            seed_item(snap, RIFTER_BP_TYPE_ID, char_id, JITA_STATION_ID, InventoryFlag::Hangar, 1, true, false, None),
        )
    });

    // 1. Blueprint copying runs cap (maxProductionLimit) & Science + Advanced Industry skill scaling
    let max_limit = ctx.sde.blueprints[&RIFTER_BP_TYPE_ID].max_production_limit;
    assert!(max_limit > 0);
    let over_limit_err = ctx.dispatch_rpc(&mut sess, "industryManager", None, "InstallJob", &[PyRep::keyval([
        ("blueprintID", PyRep::Integer(bpo_copy_1)), ("facilityID", PyRep::Integer(JITA_STATION_ID)),
        ("activityID", PyRep::Integer(5)), ("runs", PyRep::Integer((max_limit + 1) as i64)),
    ])], &kw, None).await.expect_err("Copy runs > maxProductionLimit must fail");
    assert_eq!(over_limit_err.code, "MaxRunsPerCopyExceeded");

    let copy_unskilled = ctx.dispatch_rpc(&mut sess, "industryManager", None, "InstallJob", &[PyRep::keyval([
        ("blueprintID", PyRep::Integer(bpo_copy_1)), ("facilityID", PyRep::Integer(JITA_STATION_ID)),
        ("activityID", PyRep::Integer(5)), ("runs", PyRep::Integer(1)),
    ])], &kw, None).await.expect("Install unskilled copy job");
    let copy_dur_unskilled_1 = copy_unskilled.dict_get("durationSeconds").and_then(|v| v.as_i64()).unwrap();

    let me_unskilled = ctx.dispatch_rpc(&mut sess, "industryManager", None, "InstallJob", &[PyRep::keyval([
        ("blueprintID", PyRep::Integer(bpo_me)), ("facilityID", PyRep::Integer(JITA_STATION_ID)),
        ("activityID", PyRep::Integer(4)), ("runs", PyRep::Integer(1)),
    ])], &kw, None).await.expect("Install unskilled ME job");
    let me_dur_unskilled_1 = me_unskilled.dict_get("durationSeconds").and_then(|v| v.as_i64()).unwrap();
    let me_job_1 = me_unskilled.dict_get("jobID").and_then(|v| v.as_i64()).unwrap();

    let te_unskilled = ctx.dispatch_rpc(&mut sess, "industryManager", None, "InstallJob", &[PyRep::keyval([
        ("blueprintID", PyRep::Integer(bpo_te)), ("facilityID", PyRep::Integer(JITA_STATION_ID)),
        ("activityID", PyRep::Integer(3)), ("runs", PyRep::Integer(1)),
    ])], &kw, None).await.expect("Install unskilled TE job");
    let te_dur_unskilled_1 = te_unskilled.dict_get("durationSeconds").and_then(|v| v.as_i64()).unwrap();
    let te_job_1 = te_unskilled.dict_get("jobID").and_then(|v| v.as_i64()).unwrap();

    // Train Science (3402), Research (3403), Metallurgy (3409), and Advanced Industry (3388) to level 5
    ctx.db.with_mut_snapshot(|snap| {
        let sk = snap.skills.entry(char_id).or_default();
        for stid in [3402, 3403, 3409, 3388] {
            sk.insert(stid, TrainedSkillRecord { type_id: stid, level: 5, skill_points: 256_000.0 });
        }
    });

    let copy_skilled_5 = ctx.dispatch_rpc(&mut sess, "industryManager", None, "InstallJob", &[PyRep::keyval([
        ("blueprintID", PyRep::Integer(bpo_copy_2)), ("facilityID", PyRep::Integer(JITA_STATION_ID)),
        ("activityID", PyRep::Integer(5)), ("runs", PyRep::Integer(5)),
    ])], &kw, None).await.expect("Install skilled 5-run copy job");
    let copy_dur_skilled_5 = copy_skilled_5.dict_get("durationSeconds").and_then(|v| v.as_i64()).unwrap();
    let expected_copy_1 = ((copy_dur_unskilled_1 as f64) * 0.75 * 0.85).round() as i64;
    assert_eq!(copy_dur_skilled_5, expected_copy_1 * 5);
    let copy_job_5 = copy_skilled_5.dict_get("jobID").and_then(|v| v.as_i64()).unwrap();

    ctx.advance_virtual_clock(100_000);
    let copy_done = ctx.dispatch_rpc(&mut sess, "industryManager", None, "CompleteJob", &[PyRep::Integer(copy_job_5)], &kw, None).await.expect("Complete 5-run copy job");
    let bpc_id = copy_done.dict_get("createdBpcItemID").and_then(|v| v.as_i64()).unwrap();
    let bpc_item = ctx.db.get_item(bpc_id).unwrap();
    assert!(bpc_item.is_blueprint_copy);
    assert_eq!(bpc_item.blueprint_runs_remaining, Some(5));

    ctx.dispatch_rpc(&mut sess, "industryManager", None, "CompleteJob", &[PyRep::Integer(me_job_1)], &kw, None).await.expect("Complete ME job 1");
    ctx.dispatch_rpc(&mut sess, "industryManager", None, "CompleteJob", &[PyRep::Integer(te_job_1)], &kw, None).await.expect("Complete TE job 1");
    assert_eq!(ctx.db.get_item(bpo_me).unwrap().dynamic_attributes.get(&1112).copied(), Some(1.0));
    assert_eq!(ctx.db.get_item(bpo_te).unwrap().dynamic_attributes.get(&1113).copied(), Some(2.0));

    // 2. Exponential research time + Metallurgy/Research + Advanced Industry skill reduction + ME 10 / TE 20 caps
    let me_skilled_9 = ctx.dispatch_rpc(&mut sess, "industryManager", None, "InstallJob", &[PyRep::keyval([
        ("blueprintID", PyRep::Integer(bpo_me)), ("facilityID", PyRep::Integer(JITA_STATION_ID)),
        ("activityID", PyRep::Integer(4)), ("runs", PyRep::Integer(9)),
    ])], &kw, None).await.expect("Install ME 1->10 job");
    let me_dur_skilled_9 = me_skilled_9.dict_get("durationSeconds").and_then(|v| v.as_i64()).unwrap();
    let exp_9_sum: u64 = [250, 595, 1_414, 3_360, 8_000, 19_000, 45_255, 107_700, 256_000].iter().sum();
    let expected_me_9 = ((exp_9_sum as f64) * ((me_dur_unskilled_1 as f64) / 105.0) * 0.75 * 0.85).round() as i64;
    assert_eq!(me_dur_skilled_9, expected_me_9);
    let me_job_9 = me_skilled_9.dict_get("jobID").and_then(|v| v.as_i64()).unwrap();

    let te_skilled_9 = ctx.dispatch_rpc(&mut sess, "industryManager", None, "InstallJob", &[PyRep::keyval([
        ("blueprintID", PyRep::Integer(bpo_te)), ("facilityID", PyRep::Integer(JITA_STATION_ID)),
        ("activityID", PyRep::Integer(3)), ("runs", PyRep::Integer(9)),
    ])], &kw, None).await.expect("Install TE 2->20 job");
    let te_dur_skilled_9 = te_skilled_9.dict_get("durationSeconds").and_then(|v| v.as_i64()).unwrap();
    let expected_te_9 = ((exp_9_sum as f64) * ((te_dur_unskilled_1 as f64) / 105.0) * 0.75 * 0.85).round() as i64;
    assert_eq!(te_dur_skilled_9, expected_te_9);
    let te_job_9 = te_skilled_9.dict_get("jobID").and_then(|v| v.as_i64()).unwrap();

    ctx.advance_virtual_clock(6_000_000);
    ctx.dispatch_rpc(&mut sess, "industryManager", None, "CompleteJob", &[PyRep::Integer(me_job_9)], &kw, None).await.expect("Complete ME 10 job");
    ctx.dispatch_rpc(&mut sess, "industryManager", None, "CompleteJob", &[PyRep::Integer(te_job_9)], &kw, None).await.expect("Complete TE 20 job");
    assert_eq!(ctx.db.get_item(bpo_me).unwrap().dynamic_attributes.get(&1112).copied(), Some(10.0));
    assert_eq!(ctx.db.get_item(bpo_te).unwrap().dynamic_attributes.get(&1113).copied(), Some(20.0));

    let me_cap_err = ctx.dispatch_rpc(&mut sess, "industryManager", None, "InstallJob", &[PyRep::keyval([
        ("blueprintID", PyRep::Integer(bpo_me)), ("facilityID", PyRep::Integer(JITA_STATION_ID)),
        ("activityID", PyRep::Integer(4)), ("runs", PyRep::Integer(1)),
    ])], &kw, None).await.expect_err("ME > 10 must fail");
    assert_eq!(me_cap_err.code, "MaxMaterialEfficiencyReached");

    let te_cap_err = ctx.dispatch_rpc(&mut sess, "industryManager", None, "InstallJob", &[PyRep::keyval([
        ("blueprintID", PyRep::Integer(bpo_te)), ("facilityID", PyRep::Integer(JITA_STATION_ID)),
        ("activityID", PyRep::Integer(3)), ("runs", PyRep::Integer(1)),
    ])], &kw, None).await.expect_err("TE > 20 must fail");
    assert_eq!(te_cap_err.code, "MaxTimeEfficiencyReached");

    // 3. Unresearched BPO repackaging vs researched BPO repackaging with reset confirmation
    ctx.dispatch_rpc(&mut sess, "repackagingSvc", None, "RepackageItems", &[PyRep::List(vec![PyRep::Integer(bpo_pack)])], &kw, None)
        .await.expect("Unresearched BPO repackage succeeds");
    assert!(!ctx.db.get_item(bpo_pack).unwrap().singleton);

    let repack_err = ctx.dispatch_rpc(&mut sess, "repackagingSvc", None, "RepackageItems", &[PyRep::List(vec![PyRep::Integer(bpo_me)])], &kw, None)
        .await.expect_err("Researched BPO repackage without reset must fail");
    assert_eq!(repack_err.code, "CannotRepackageMutatedItem");

    ctx.dispatch_rpc(&mut sess, "repackagingSvc", None, "RepackageItems", &[PyRep::List(vec![PyRep::Integer(bpo_me)]), PyRep::Bool(true)], &kw, None)
        .await.expect("Researched BPO repackage with reset confirmation succeeds");
    let reset_bpo = ctx.db.get_item(bpo_me).unwrap();
    assert!(!reset_bpo.singleton);
    assert_eq!(reset_bpo.dynamic_attributes.get(&1112).copied().unwrap_or(0.0), 0.0);
    assert_eq!(reset_bpo.dynamic_attributes.get(&1113).copied().unwrap_or(0.0), 0.0);
}

#[tokio::test]
async fn test_industry_capital_components_restrictions_and_core_temp() {
    // parity: feat:industry.capital-component-construction, feat:industry.capital-construction-restrictions, feat:industry.capital-core-temperature-components
    eve_testkit::attest(&[
        "feat:industry.capital-component-construction",
        "feat:industry.capital-construction-restrictions",
        "feat:industry.capital-core-temperature-components",
    ]);
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let kw = HashMap::new();
    let char_id = 90_000_001;
    let corp_id = 98_000_711;
    let alliance_id = 99_000_711;
    let mut sess = setup_pilot(&ctx, 702, "demo", "demo-secret", char_id).await;

    let hs_athanor_id = 1_045_000_000_700_i64;
    let ls_tatara_id = 1_045_000_000_701_i64;
    let ls_azbel_id = 1_045_000_000_702_i64;
    let ns_sotiyo_id = 1_045_000_000_703_i64;
    let (
        reaction_bp_id,
        core_temp_bp_id,
        cap_comp_bp_id,
        archon_bp_hs_id,
        archon_bp_ls_id,
        erebus_bp_ls_id,
        erebus_bp_ns_id,
    ) = ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.corporation_id = corp_id;
            ch.alliance_id = Some(alliance_id);
            ch.balance = IskCents::from_isk(500_000_000_000);
        }
        snap.structures.insert(hs_athanor_id, make_structure(hs_athanor_id, 35835, corp_id, JITA_SOLAR_SYSTEM_ID, "HS Athanor"));
        snap.structures.insert(ls_tatara_id, make_structure(ls_tatara_id, 35836, corp_id, LOWSEC_SOLAR_SYSTEM_ID, "LS Tatara"));
        snap.structures.insert(ls_azbel_id, make_structure(ls_azbel_id, 35826, corp_id, LOWSEC_SOLAR_SYSTEM_ID, "LS Azbel"));
        snap.structures.insert(ns_sotiyo_id, make_structure(ns_sotiyo_id, 35827, corp_id, NULLSEC_SOLAR_SYSTEM_ID, "NS Sotiyo"));
        for (sid, svc_type) in [(hs_athanor_id, 35894), (ls_tatara_id, 35894), (ls_azbel_id, 35877), (ns_sotiyo_id, 35877)] {
            let svc = seed_item(snap, svc_type, corp_id, sid, InventoryFlag::ServiceSlot0, 1, true, false, None);
            snap.items.get_mut(&svc).unwrap().dynamic_attributes.insert(2, 1.0);
        }

        let sk = snap.skills.entry(char_id).or_default();
        for (stid, lvl) in [(3380, 5), (3387, 5), (24625, 5), (45746, 5), (45748, 5), (45749, 5), (22242, 5)] {
            sk.insert(stid, TrainedSkillRecord { type_id: stid, level: lvl, skill_points: 256_000.0 });
        }
        (
            seed_item(snap, 57495, char_id, ls_tatara_id, InventoryFlag::Hangar, 1, true, false, None),
            seed_item(snap, 57524, char_id, LOWSEC_STATION_ID, InventoryFlag::Hangar, 1, true, false, None),
            seed_item(snap, 21010, char_id, LOWSEC_STATION_ID, InventoryFlag::Hangar, 1, true, false, None),
            seed_item(snap, 23758, char_id, JITA_STATION_ID, InventoryFlag::Hangar, 1, true, false, None),
            seed_item(snap, 23758, char_id, ls_azbel_id, InventoryFlag::Hangar, 1, true, false, None),
            seed_item(snap, 1002, char_id, ls_azbel_id, InventoryFlag::Hangar, 1, true, false, None),
            seed_item(snap, 1002, char_id, ns_sotiyo_id, InventoryFlag::Hangar, 1, true, false, None),
        )
    });

    // 1. Capital component construction: Reactions blocked in high-sec, allowed in low-sec/null-sec
    let hs_react_bp = ctx.db.with_mut_snapshot(|snap| {
        seed_item(snap, 57495, char_id, hs_athanor_id, InventoryFlag::Hangar, 1, true, false, None)
    });
    let hs_react_err = ctx
        .dispatch_rpc(
            &mut sess,
            "industryManager",
            None,
            "InstallJob",
            &[PyRep::keyval([
                ("blueprintID", PyRep::Integer(hs_react_bp)),
                ("facilityID", PyRep::Integer(hs_athanor_id)),
                ("activityID", PyRep::Integer(9)),
                ("runs", PyRep::Integer(1)),
            ])],
            &kw,
            None,
        )
        .await
        .expect_err("Reaction in high-sec must fail");
    assert_eq!(hs_react_err.code, "ReactionsForbiddenInHighSec");

    let core_temp_mats = ctx.sde.blueprints[&57524].activities["manufacturing"].materials.clone();
    assert!(core_temp_mats.iter().any(|m| matches!(m.type_id, 2867 | 2868 | 2870 | 2872 | 2875)));

    ctx.db.with_mut_snapshot(|snap| {
        for (bp_tid, loc) in [(57495_u32, ls_tatara_id), (57524, LOWSEC_STATION_ID), (21010, LOWSEC_STATION_ID)] {
            let bp = &ctx.sde.blueprints[&bp_tid];
            let act = bp.activities.get("manufacturing").or_else(|| bp.activities.get("reaction")).unwrap();
            for m in &act.materials {
                seed_item(snap, m.type_id, char_id, loc, InventoryFlag::Hangar, (m.quantity * 2) as i32, false, false, None);
            }
            for sk_req in &act.skills {
                snap.skills.entry(char_id).or_default().insert(
                    sk_req.type_id,
                    TrainedSkillRecord { type_id: sk_req.type_id, level: 5, skill_points: 256_000.0 },
                );
            }
        }
    });

    move_pilot(&ctx, &mut sess, char_id, LOWSEC_SOLAR_SYSTEM_ID, None, Some(ls_tatara_id));
    let react_job = ctx
        .dispatch_rpc(
            &mut sess,
            "industryManager",
            None,
            "InstallJob",
            &[PyRep::keyval([
                ("blueprintID", PyRep::Integer(reaction_bp_id)),
                ("facilityID", PyRep::Integer(ls_tatara_id)),
                ("activityID", PyRep::Integer(9)),
                ("runs", PyRep::Integer(1)),
            ])],
            &kw,
            None,
        )
        .await
        .expect("Install Neurolink Protection Cell reaction in low-sec");

    move_pilot(&ctx, &mut sess, char_id, LOWSEC_SOLAR_SYSTEM_ID, Some(LOWSEC_STATION_ID), None);
    let core_temp_job = ctx
        .dispatch_rpc(
            &mut sess,
            "industryManager",
            None,
            "InstallJob",
            &[PyRep::keyval([
                ("blueprintID", PyRep::Integer(core_temp_bp_id)),
                ("facilityID", PyRep::Integer(LOWSEC_STATION_ID)),
                ("activityID", PyRep::Integer(1)),
                ("runs", PyRep::Integer(1)),
            ])],
            &kw,
            None,
        )
        .await
        .expect("Install Core Temperature Regulator manufacturing job");
    let cap_comp_job = ctx
        .dispatch_rpc(
            &mut sess,
            "industryManager",
            None,
            "InstallJob",
            &[PyRep::keyval([
                ("blueprintID", PyRep::Integer(cap_comp_bp_id)),
                ("facilityID", PyRep::Integer(LOWSEC_STATION_ID)),
                ("activityID", PyRep::Integer(1)),
                ("runs", PyRep::Integer(1)),
            ])],
            &kw,
            None,
        )
        .await
        .expect("Install Capital Propulsion Engine manufacturing job");

    ctx.advance_virtual_clock(500_000);
    for j in [&react_job, &core_temp_job, &cap_comp_job] {
        let jid = j.dict_get("jobID").and_then(|v| v.as_i64()).unwrap();
        ctx.dispatch_rpc(&mut sess, "industryManager", None, "CompleteJob", &[PyRep::Integer(jid)], &kw, None)
            .await
            .expect("Complete capital component / reaction job");
    }

    // 2. Capital construction restrictions & Core Temperature / Neurolink / Capital Construction Parts BOM
    move_pilot(&ctx, &mut sess, char_id, JITA_SOLAR_SYSTEM_ID, Some(JITA_STATION_ID), None);
    let hs_cap_err = ctx
        .dispatch_rpc(
            &mut sess,
            "industryManager",
            None,
            "InstallJob",
            &[PyRep::keyval([
                ("blueprintID", PyRep::Integer(archon_bp_hs_id)),
                ("facilityID", PyRep::Integer(JITA_STATION_ID)),
                ("activityID", PyRep::Integer(1)),
                ("runs", PyRep::Integer(1)),
            ])],
            &kw,
            None,
        )
        .await
        .expect_err("Capital ship in high-sec must fail");
    assert_eq!(hs_cap_err.code, "CapitalConstructionForbiddenInHighSec");

    let archon_act = &ctx.sde.blueprints[&23758].activities["manufacturing"];
    assert!(archon_act.materials.iter().any(|m| m.type_id == 57487)); // Core Temperature Regulator
    assert!(archon_act.materials.iter().any(|m| m.type_id == 57488)); // Neurolink Protection Cell
    assert!(archon_act.materials.iter().any(|m| m.type_id == 21009)); // Capital Propulsion Engine

    move_pilot(&ctx, &mut sess, char_id, LOWSEC_SOLAR_SYSTEM_ID, None, Some(ls_azbel_id));
    ctx.db.with_mut_snapshot(|snap| {
        for sk_req in &archon_act.skills {
            snap.skills.entry(char_id).or_default().insert(
                sk_req.type_id,
                TrainedSkillRecord { type_id: sk_req.type_id, level: 5, skill_points: 256_000.0 },
            );
        }
        for m in &archon_act.materials {
            if m.type_id != 57487 {
                seed_item(snap, m.type_id, char_id, ls_azbel_id, InventoryFlag::Hangar, (m.quantity * 2) as i32, false, false, None);
            }
        }
    });
    let missing_ctr_err = ctx
        .dispatch_rpc(
            &mut sess,
            "industryManager",
            None,
            "InstallJob",
            &[PyRep::keyval([
                ("blueprintID", PyRep::Integer(archon_bp_ls_id)),
                ("facilityID", PyRep::Integer(ls_azbel_id)),
                ("activityID", PyRep::Integer(1)),
                ("runs", PyRep::Integer(1)),
            ])],
            &kw,
            None,
        )
        .await
        .expect_err("Capital build missing Core Temperature Regulator must fail");
    assert_eq!(missing_ctr_err.code, "InsufficientManufacturingMaterials");

    ctx.db.with_mut_snapshot(|snap| {
        seed_item(snap, 57487, char_id, ls_azbel_id, InventoryFlag::Hangar, 2, false, false, None);
    });
    let archon_job = ctx
        .dispatch_rpc(
            &mut sess,
            "industryManager",
            None,
            "InstallJob",
            &[PyRep::keyval([
                ("blueprintID", PyRep::Integer(archon_bp_ls_id)),
                ("facilityID", PyRep::Integer(ls_azbel_id)),
                ("activityID", PyRep::Integer(1)),
                ("runs", PyRep::Integer(1)),
            ])],
            &kw,
            None,
        )
        .await
        .expect("Archon build in low-sec Azbel succeeds with full capital BOM");
    let archon_jid = archon_job.dict_get("jobID").and_then(|v| v.as_i64()).unwrap();
    ctx.advance_virtual_clock(2_000_000);
    ctx.dispatch_rpc(&mut sess, "industryManager", None, "CompleteJob", &[PyRep::Integer(archon_jid)], &kw, None)
        .await
        .expect("Complete Archon job");

    // Supercapital (Erebus Titan 1002 BP) rejected in low-sec and in null-sec without sovereign Supercapital Construction Facilities upgrade
    let ls_super_err = ctx
        .dispatch_rpc(
            &mut sess,
            "industryManager",
            None,
            "InstallJob",
            &[PyRep::keyval([
                ("blueprintID", PyRep::Integer(erebus_bp_ls_id)),
                ("facilityID", PyRep::Integer(ls_azbel_id)),
                ("activityID", PyRep::Integer(1)),
                ("runs", PyRep::Integer(1)),
            ])],
            &kw,
            None,
        )
        .await
        .expect_err("Supercapital in low-sec must fail");
    assert_eq!(ls_super_err.code, "SupercapitalRequiresSovereignNullSec");

    move_pilot(&ctx, &mut sess, char_id, NULLSEC_SOLAR_SYSTEM_ID, None, Some(ns_sotiyo_id));
    let ns_no_upgrade_err = ctx
        .dispatch_rpc(
            &mut sess,
            "industryManager",
            None,
            "InstallJob",
            &[PyRep::keyval([
                ("blueprintID", PyRep::Integer(erebus_bp_ns_id)),
                ("facilityID", PyRep::Integer(ns_sotiyo_id)),
                ("activityID", PyRep::Integer(1)),
                ("runs", PyRep::Integer(1)),
            ])],
            &kw,
            None,
        )
        .await
        .expect_err("Supercapital in null-sec without sov hub supercapital upgrade must fail");
    assert_eq!(ns_no_upgrade_err.code, "SupercapitalRequiresSovereignNullSec");

    let erebus_act = &ctx.sde.blueprints[&1002].activities["manufacturing"];
    ctx.db.with_mut_snapshot(|snap| {
        snap.system_states.entry(NULLSEC_SOLAR_SYSTEM_ID).or_default().sov_alliance_id = Some(alliance_id);
        let upg = seed_item(snap, 81623, corp_id, NULLSEC_SOLAR_SYSTEM_ID as i64, InventoryFlag::Hangar, 1, true, false, None);
        snap.items.get_mut(&upg).unwrap().dynamic_attributes.insert(2, 1.0);
        for sk_req in &erebus_act.skills {
            snap.skills.entry(char_id).or_default().insert(
                sk_req.type_id,
                TrainedSkillRecord { type_id: sk_req.type_id, level: 5, skill_points: 256_000.0 },
            );
        }
        for m in &erebus_act.materials {
            seed_item(snap, m.type_id, char_id, ns_sotiyo_id, InventoryFlag::Hangar, (m.quantity * 2) as i32, false, false, None);
        }
    });
    let erebus_job = ctx
        .dispatch_rpc(
            &mut sess,
            "industryManager",
            None,
            "InstallJob",
            &[PyRep::keyval([
                ("blueprintID", PyRep::Integer(erebus_bp_ns_id)),
                ("facilityID", PyRep::Integer(ns_sotiyo_id)),
                ("activityID", PyRep::Integer(1)),
                ("runs", PyRep::Integer(1)),
            ])],
            &kw,
            None,
        )
        .await
        .expect("Supercapital build in sovereign null-sec with online supercap upgrade succeeds");
    assert!(erebus_job.dict_get("jobID").and_then(|v| v.as_i64()).unwrap() > 0);
}
