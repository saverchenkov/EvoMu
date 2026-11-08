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
    // parity: feat:industry.blueprint-copying-runs, feat:industry.blueprint-te-research, feat:industry.blueprint-pack-unpack
    eve_testkit::attest(&[
        "feat:industry.blueprint-copying-runs",
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
    // parity: feat:industry.capital-component-construction, feat:industry.capital-core-temperature-components
    eve_testkit::attest(&[
        "feat:industry.capital-component-construction",
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

pub(crate) mod pi_command_center_budget_launchpads {
    // Extended Planetary Interaction, Customs Offices, and Wallet/Item Trader parity tests (Part 2).

    use crate::common;
    use eve_db::{InventoryFlag, InventoryItemRecord, IskCents, TrainedSkillRecord, TypeId};
    use eve_machonet::SessionState;
    use eve_proto::PyRep;
    use eve_server::router::ServerContext;
    use std::collections::HashMap;

    const JITA_SOLAR_SYSTEM_ID: u32 = 30_000_142;
    const JITA_PLANET_ID: i64 = 40_009_082; // Jita IV (Gas)

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
    async fn test_pi_command_center_budget_launchpads_and_planet_resources() {
        // parity: feat:pi.command-center-cpu-power-budget, feat:pi.command-center-import-export-pad, feat:pi.planet-type-resource-restrictions
        eve_testkit::attest(&[
            "feat:pi.command-center-cpu-power-budget",
            "feat:pi.command-center-import-export-pad",
            "feat:pi.planet-type-resource-restrictions",
        ]);
        let ctx = ServerContext::new();
        crate::common::pin_clock(&ctx);
        let kw = HashMap::new();
        let char_id = 90_000_001;
        let mut sess = setup_pilot(&ctx, 705, "demo", "demo-secret", char_id).await;
        sess.solar_system_id = Some(JITA_SOLAR_SYSTEM_ID);
        sess.station_id = None;
        let ship_id = sess.ship_id.expect("Active ship");

        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&char_id) {
                ch.station_id = None;
                ch.balance = IskCents::from_isk(100_000_000);
            }
            seed_item(snap, 2534, char_id, ship_id, InventoryFlag::Cargo, 1, false, false, None);
            let sk = snap.skills.entry(char_id).or_default();
            sk.insert(2505, TrainedSkillRecord { type_id: 2505, level: 2, skill_points: 8_000.0 });
            sk.insert(2495, TrainedSkillRecord { type_id: 2495, level: 5, skill_points: 256_000.0 });
        });

        // 1. Planet type resource restrictions
        let gas_res = ctx
            .dispatch_rpc(&mut sess, "planetMgr", None, "GetPlanetResourceInfo", &[PyRep::Integer(JITA_PLANET_ID)], &kw, None)
            .await
            .expect("Gas planet resources");
        let PyRep::List(gas_list) = gas_res.dict_get("resources").unwrap() else { panic!() };
        let gas_tids: Vec<TypeId> = gas_list
            .iter()
            .filter_map(|r| r.dict_get("resourceTypeID").and_then(|v| v.as_i64()))
            .map(|v| v as TypeId)
            .collect();
        assert!(gas_tids.contains(&2310));
        assert!(!gas_tids.contains(&2307));
        assert!(!gas_tids.contains(&2286));

        // 2. Command Center CPU/PG budget gated by Command Center Upgrades skill
        let deploy = ctx
            .dispatch_rpc(
                &mut sess,
                "planetMgr",
                None,
                "UserUpdateNetwork",
                &[
                    PyRep::Integer(JITA_PLANET_ID),
                    PyRep::List(vec![PyRep::Tuple(vec![
                        PyRep::Integer(1),
                        PyRep::Integer(101),
                        PyRep::Integer(2534),
                        PyRep::Real(0.10),
                        PyRep::Real(0.10),
                    ])]),
                ],
                &kw,
                None,
            )
            .await
            .expect("Deploy Gas Command Center");
        assert_eq!(deploy.dict_get("cpuCapacity"), Some(&PyRep::Integer(1675)));
        assert_eq!(deploy.dict_get("powerCapacity"), Some(&PyRep::Integer(6000)));

        let skill_err = ctx
            .dispatch_rpc(&mut sess, "planetMgr", None, "UpgradeCommandCenter", &[PyRep::Integer(JITA_PLANET_ID), PyRep::Integer(3)], &kw, None)
            .await
            .expect_err("Upgrade above skill level must fail");
        assert_eq!(skill_err.code, "MissingSkillForCommandCenterUpgrade");

        let pg_err = ctx
            .dispatch_rpc(
                &mut sess,
                "planetMgr",
                None,
                "UserUpdateNetwork",
                &[
                    PyRep::Integer(JITA_PLANET_ID),
                    PyRep::List(vec![
                        PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(102), PyRep::Integer(3060), PyRep::Real(0.11), PyRep::Real(0.11)]),
                        PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(103), PyRep::Integer(3060), PyRep::Real(0.12), PyRep::Real(0.12)]),
                        PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(104), PyRep::Integer(3060), PyRep::Real(0.13), PyRep::Real(0.13)]),
                    ]),
                ],
                &kw,
                None,
            )
            .await
            .expect_err("Exceeding Command Center PG budget must fail");
        assert_eq!(pg_err.code, "InsufficientColonyCpuOrPowergrid");

        ctx.db.with_mut_snapshot(|snap| {
            snap.skills.entry(char_id).or_default().insert(
                2505,
                TrainedSkillRecord { type_id: 2505, level: 5, skill_points: 256_000.0 },
            );
        });
        let up5 = ctx
            .dispatch_rpc(&mut sess, "planetMgr", None, "UpgradeCommandCenter", &[PyRep::Integer(JITA_PLANET_ID), PyRep::Integer(5)], &kw, None)
            .await
            .expect("Upgrade Command Center to level 5");
        assert_eq!(up5.dict_get("cpuMax"), Some(&PyRep::Real(25_415.0)));
        assert_eq!(up5.dict_get("powergridMax"), Some(&PyRep::Real(19_000.0)));

        let non_native_err = ctx
            .dispatch_rpc(
                &mut sess,
                "planetMgr",
                None,
                "UserUpdateNetwork",
                &[
                    PyRep::Integer(JITA_PLANET_ID),
                    PyRep::List(vec![
                        PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(102), PyRep::Integer(3060), PyRep::Real(0.11), PyRep::Real(0.11)]),
                        PyRep::Tuple(vec![PyRep::Integer(10), PyRep::Integer(102), PyRep::Integer(2307), PyRep::Integer(300), PyRep::Integer(1)]),
                    ]),
                ],
                &kw,
                None,
            )
            .await
            .expect_err("Extracting non-native P0 resource must fail");
        assert_eq!(non_native_err.code, "ClientSchematicOverrideForbidden");

        // 3. Command Center 500 m3 one-way orbital launch (at 1.5x higher tax) vs Spaceport Launchpad 10,000 m3 bidirectional transfer
        ctx.db.with_mut_snapshot(|snap| {
            seed_item(snap, 3645, char_id, ship_id, InventoryFlag::Cargo, 100, false, false, None);
            let mut net = eve_server::services::industry_pi::load_pi_network(snap, char_id, JITA_PLANET_ID, 0);
            if let Some(pin) = net.pins.first_mut() {
                pin.contents.insert(3645, 1_000);
            }
            eve_server::services::industry_pi::save_pi_network(snap, char_id, JITA_PLANET_ID, &net);
        });
        let cc_cap_err = ctx
            .dispatch_rpc(
                &mut sess,
                "planetMgr",
                None,
                "UserLaunchCommodities",
                &[PyRep::Integer(JITA_PLANET_ID), PyRep::Integer(3645), PyRep::Integer(3_000), PyRep::String("CommandCenter".into())],
                &kw,
                None,
            )
            .await
            .expect_err("Command Center launch > 500 m3 must fail");
        assert_eq!(cc_cap_err.code, "CommandCenterLaunchCapacityExceeded");

        let cc_launch = ctx
            .dispatch_rpc(
                &mut sess,
                "planetMgr",
                None,
                "UserLaunchCommodities",
                &[PyRep::Integer(JITA_PLANET_ID), PyRep::Integer(3645), PyRep::Integer(100), PyRep::String("CommandCenter".into())],
                &kw,
                None,
            )
            .await
            .expect("Valid Command Center launch");
        let cc_tax = cc_launch.dict_get("exportTaxISK").and_then(|v| v.as_f64()).unwrap();

        let lp_cap_err = ctx
            .dispatch_rpc(
                &mut sess,
                "planetMgr",
                None,
                "UserLaunchCommodities",
                &[PyRep::Integer(JITA_PLANET_ID), PyRep::Integer(3645), PyRep::Integer(60_000), PyRep::String("CustomsOffice".into())],
                &kw,
                None,
            )
            .await
            .expect_err("Launchpad transfer > 10,000 m3 must fail");
        assert_eq!(lp_cap_err.code, "LaunchpadCapacityExceeded");

        let lp_export = ctx
            .dispatch_rpc(
                &mut sess,
                "planetMgr",
                None,
                "UserLaunchCommodities",
                &[PyRep::Integer(JITA_PLANET_ID), PyRep::Integer(3645), PyRep::Integer(100), PyRep::String("CustomsOffice".into())],
                &kw,
                None,
            )
            .await
            .expect("Valid Launchpad export");
        let lp_export_tax = lp_export.dict_get("exportTaxISK").and_then(|v| v.as_f64()).unwrap();
        assert!((cc_tax - lp_export_tax * 1.5).abs() < 1e-4);

        let lp_import = ctx
            .dispatch_rpc(
                &mut sess,
                "planetMgr",
                None,
                "UserLaunchCommodities",
                &[PyRep::Integer(JITA_PLANET_ID), PyRep::Integer(3645), PyRep::Integer(100), PyRep::String("ImportToColony".into())],
                &kw,
                None,
            )
            .await
            .expect("Valid Launchpad import");
        let lp_import_tax = lp_import.dict_get("importTaxISK").and_then(|v| v.as_f64()).unwrap();
        assert!((lp_import_tax - lp_export_tax * 0.5).abs() < 1e-4);
    }

    #[tokio::test]
    async fn test_pi_extractor_heads_decay_and_schematic_routing() {
        // parity: feat:pi.extractor-head-placement-and-decay, feat:pi.schematic-input-output-routing
        eve_testkit::attest(&[
            "feat:pi.extractor-head-placement-and-decay",
            "feat:pi.schematic-input-output-routing",
        ]);
        let ctx = ServerContext::new();
        crate::common::pin_clock(&ctx);
        let kw = HashMap::new();
        let char_id = 90_000_001;
        let mut sess = setup_pilot(&ctx, 706, "demo", "demo-secret", char_id).await;
        sess.solar_system_id = Some(JITA_SOLAR_SYSTEM_ID);
        sess.station_id = None;
        let ship_id = sess.ship_id.expect("Active ship");

        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&char_id) {
                ch.station_id = None;
                ch.balance = IskCents::from_isk(100_000_000);
            }
            seed_item(snap, 2534, char_id, ship_id, InventoryFlag::Cargo, 1, false, false, None);
            snap.skills.entry(char_id).or_default().insert(
                2505,
                TrainedSkillRecord { type_id: 2505, level: 5, skill_points: 256_000.0 },
            );
        });

        let setup = ctx
            .dispatch_rpc(
                &mut sess,
                "planetMgr",
                None,
                "UserUpdateNetwork",
                &[
                    PyRep::Integer(JITA_PLANET_ID),
                    PyRep::List(vec![
                        PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(101), PyRep::Integer(2534), PyRep::Real(0.10), PyRep::Real(0.10)]),
                        PyRep::Tuple(vec![PyRep::Integer(9), PyRep::Integer(101), PyRep::Integer(5)]),
                        PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(102), PyRep::Integer(3060), PyRep::Real(0.11), PyRep::Real(0.11)]),
                        PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(103), PyRep::Integer(2492), PyRep::Real(0.12), PyRep::Real(0.12)]),
                        PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(104), PyRep::Integer(2494), PyRep::Real(0.13), PyRep::Real(0.13)]),
                        PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(105), PyRep::Integer(2482), PyRep::Real(0.14), PyRep::Real(0.14)]),
                        PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(106), PyRep::Integer(2541), PyRep::Real(0.15), PyRep::Real(0.15)]),
                        PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(107), PyRep::Integer(2543), PyRep::Real(0.16), PyRep::Real(0.16)]),
                    ]),
                ],
                &kw,
                None,
            )
            .await
            .expect("Deploy full PI facility network");
        let PyRep::List(pins) = setup.dict_get("pins").unwrap() else { panic!() };
        let ext_id = pins[1].dict_get("pinID").and_then(|v| v.as_i64()).unwrap();
        let basic_id = pins[2].dict_get("pinID").and_then(|v| v.as_i64()).unwrap();
        let adv_id = pins[3].dict_get("pinID").and_then(|v| v.as_i64()).unwrap();
        let htech_id = pins[4].dict_get("pinID").and_then(|v| v.as_i64()).unwrap();
        let store_id = pins[5].dict_get("pinID").and_then(|v| v.as_i64()).unwrap();
        let pad_id = pins[6].dict_get("pinID").and_then(|v| v.as_i64()).unwrap();

        // 1. Extractor head placement: max 10 heads, radius check, overlap interference, and per-cycle decay
        let over_10_err = ctx
            .dispatch_rpc(
                &mut sess,
                "planetMgr",
                None,
                "UserUpdateNetwork",
                &[
                    PyRep::Integer(JITA_PLANET_ID),
                    PyRep::List(vec![PyRep::Tuple(vec![
                        PyRep::Integer(10),
                        PyRep::Integer(ext_id),
                        PyRep::Integer(2268),
                        PyRep::Integer(300),
                        PyRep::Integer(11),
                    ])]),
                ],
                &kw,
                None,
            )
            .await
            .expect_err("> 10 extractor heads must fail");
        assert_eq!(over_10_err.code, "InvalidExtractorHeadCount");

        let out_of_range_err = ctx
            .dispatch_rpc(
                &mut sess,
                "planetMgr",
                None,
                "UserUpdateNetwork",
                &[
                    PyRep::Integer(JITA_PLANET_ID),
                    PyRep::List(vec![PyRep::Tuple(vec![
                        PyRep::Integer(14),
                        PyRep::Integer(ext_id),
                        PyRep::Integer(1),
                        PyRep::Real(1.20),
                        PyRep::Real(1.20),
                    ])]),
                ],
                &kw,
                None,
            )
            .await
            .expect_err("Head outside ECU radius must fail");
        assert_eq!(out_of_range_err.code, "ExtractorHeadOutOfRange");

        ctx.dispatch_rpc(
            &mut sess,
            "planetMgr",
            None,
            "UserUpdateNetwork",
            &[
                PyRep::Integer(JITA_PLANET_ID),
                PyRep::List(vec![PyRep::Tuple(vec![
                    PyRep::Integer(10),
                    PyRep::Integer(ext_id),
                    PyRep::Integer(2268),
                    PyRep::Real(0.03),
                    PyRep::Integer(2),
                    PyRep::List(vec![
                        PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Real(0.11), PyRep::Real(0.11)]),
                        PyRep::Tuple(vec![PyRep::Integer(2), PyRep::Real(0.20), PyRep::Real(0.20)]),
                    ]),
                ])]),
            ],
            &kw,
            None,
        )
        .await
        .expect("Install separated 2-head program");
        let prog_sep = ctx
            .dispatch_rpc(&mut sess, "planetMgr", None, "GetProgramResultInfo", &[PyRep::Integer(JITA_PLANET_ID), PyRep::Integer(ext_id)], &kw, None)
            .await
            .expect("GetProgramResultInfo separated");
        let PyRep::Tuple(sep_tuple) = &prog_sep else { panic!() };
        let sep_c0 = sep_tuple[0].as_i64().unwrap();
        assert!(sep_c0 > 0);
        assert_eq!(sep_tuple[2].as_i64(), Some(10));

        ctx.dispatch_rpc(
            &mut sess,
            "planetMgr",
            None,
            "UserUpdateNetwork",
            &[
                PyRep::Integer(JITA_PLANET_ID),
                PyRep::List(vec![PyRep::Tuple(vec![
                    PyRep::Integer(15),
                    PyRep::Integer(ext_id),
                    PyRep::Integer(2),
                    PyRep::Real(0.111),
                    PyRep::Real(0.111),
                ])]),
            ],
            &kw,
            None,
        )
        .await
        .expect("Move head 2 to overlap head 1");
        let prog_ovl = ctx
            .dispatch_rpc(&mut sess, "planetMgr", None, "GetProgramResultInfo", &[PyRep::Integer(JITA_PLANET_ID), PyRep::Integer(ext_id)], &kw, None)
            .await
            .expect("GetProgramResultInfo overlapping");
        let PyRep::Tuple(ovl_tuple) = &prog_ovl else { panic!() };
        let ovl_c0 = ovl_tuple[0].as_i64().unwrap();
        assert!(ovl_c0 < sep_c0);

        // 2. Schematic input/output routing across Basic (P0->P1), Advanced (P1->P2), and High-Tech (P3->P4)
        let non_fac_err = ctx
            .dispatch_rpc(
                &mut sess,
                "planetMgr",
                None,
                "UserUpdateNetwork",
                &[
                    PyRep::Integer(JITA_PLANET_ID),
                    PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(7), PyRep::Integer(store_id), PyRep::Integer(66)])]),
                ],
                &kw,
                None,
            )
            .await
            .expect_err("Installing schematic on Storage pin must fail");
        assert_eq!(non_fac_err.code, "InvalidSchematic");

        let find_schem = |out_tid: u32, pin_tid: u32| -> i64 {
            ctx.sde
                .planet_schematics
                .iter()
                .find(|(_, s)| {
                    (s.pins.is_empty() || s.pins.contains(&pin_tid))
                        && s.types.iter().any(|t| !t.is_input && t.type_id == out_tid)
                })
                .map(|(&id, _)| i64::from(id))
                .unwrap()
        };
        let nano_schem = find_schem(2875, 2482);
        ctx.dispatch_rpc(
            &mut sess,
            "planetMgr",
            None,
            "UserUpdateNetwork",
            &[
                PyRep::Integer(JITA_PLANET_ID),
                PyRep::List(vec![
                    PyRep::Tuple(vec![PyRep::Integer(7), PyRep::Integer(basic_id), PyRep::Integer(find_schem(3645, 2492))]),
                    PyRep::Tuple(vec![PyRep::Integer(7), PyRep::Integer(adv_id), PyRep::Integer(find_schem(9832, 2494))]),
                    PyRep::Tuple(vec![PyRep::Integer(7), PyRep::Integer(htech_id), PyRep::Integer(nano_schem)]),
                ]),
            ],
            &kw,
            None,
        )
        .await
        .expect("Install P1, P2, and P4 schematics");

        let nano_inputs: Vec<(u32, u64)> = ctx
            .sde
            .planet_schematics
            .get(&(nano_schem as u32))
            .expect("Nano-Factory schematic in SDE")
            .types
            .iter()
            .filter(|t| t.is_input)
            .map(|t| (t.type_id, t.quantity))
            .collect();

        ctx.db.with_mut_snapshot(|snap| {
            let mut net = eve_server::services::industry_pi::load_pi_network(snap, char_id, JITA_PLANET_ID, 5);
            if let Some(pin) = net.pins.iter_mut().find(|p| p.pin_id == store_id) {
                pin.contents.insert(2268, 6_000);
                pin.contents.insert(3645, 80);
                pin.contents.insert(2390, 80);
                for &(in_tid, in_qty) in &nano_inputs {
                    pin.contents.insert(in_tid, in_qty * 2);
                }
            }
            eve_server::services::industry_pi::save_pi_network(snap, char_id, JITA_PLANET_ID, &net);
            if let Some(col) = snap.planetary_colonies.get_mut(&(char_id, JITA_PLANET_ID)) {
                col.last_extracted_unix = ctx.now_unix();
            }
        });
        ctx.advance_virtual_clock(3_600);
        let col_unrouted = ctx
            .dispatch_rpc(&mut sess, "planetMgr", None, "GetColony", &[PyRep::Integer(JITA_PLANET_ID)], &kw, None)
            .await
            .expect("GetColony without routes");
        assert_eq!(col_unrouted.dict_get("simulatedProcessedQuantity"), Some(&PyRep::Integer(0)));

        let mut route_cmds = vec![
            PyRep::Tuple(vec![PyRep::Integer(3), PyRep::Integer(store_id), PyRep::Integer(basic_id), PyRep::Integer(2)]),
            PyRep::Tuple(vec![PyRep::Integer(3), PyRep::Integer(basic_id), PyRep::Integer(pad_id), PyRep::Integer(2)]),
            PyRep::Tuple(vec![PyRep::Integer(3), PyRep::Integer(store_id), PyRep::Integer(adv_id), PyRep::Integer(2)]),
            PyRep::Tuple(vec![PyRep::Integer(3), PyRep::Integer(adv_id), PyRep::Integer(pad_id), PyRep::Integer(2)]),
            PyRep::Tuple(vec![PyRep::Integer(3), PyRep::Integer(store_id), PyRep::Integer(htech_id), PyRep::Integer(2)]),
            PyRep::Tuple(vec![PyRep::Integer(3), PyRep::Integer(htech_id), PyRep::Integer(pad_id), PyRep::Integer(2)]),
            PyRep::Tuple(vec![PyRep::Integer(6), PyRep::Integer(1), PyRep::List(vec![PyRep::Integer(store_id), PyRep::Integer(basic_id)]), PyRep::Integer(2268), PyRep::Integer(3_000)]),
            PyRep::Tuple(vec![PyRep::Integer(6), PyRep::Integer(2), PyRep::List(vec![PyRep::Integer(basic_id), PyRep::Integer(pad_id)]), PyRep::Integer(3645), PyRep::Integer(20)]),
            PyRep::Tuple(vec![PyRep::Integer(6), PyRep::Integer(3), PyRep::List(vec![PyRep::Integer(store_id), PyRep::Integer(adv_id)]), PyRep::Integer(3645), PyRep::Integer(40)]),
            PyRep::Tuple(vec![PyRep::Integer(6), PyRep::Integer(4), PyRep::List(vec![PyRep::Integer(store_id), PyRep::Integer(adv_id)]), PyRep::Integer(2390), PyRep::Integer(40)]),
            PyRep::Tuple(vec![PyRep::Integer(6), PyRep::Integer(5), PyRep::List(vec![PyRep::Integer(adv_id), PyRep::Integer(pad_id)]), PyRep::Integer(9832), PyRep::Integer(5)]),
            PyRep::Tuple(vec![PyRep::Integer(6), PyRep::Integer(9), PyRep::List(vec![PyRep::Integer(htech_id), PyRep::Integer(pad_id)]), PyRep::Integer(2875), PyRep::Integer(1)]),
        ];
        for (idx, &(in_tid, in_qty)) in nano_inputs.iter().enumerate() {
            route_cmds.push(PyRep::Tuple(vec![
                PyRep::Integer(6),
                PyRep::Integer(10 + idx as i64),
                PyRep::List(vec![PyRep::Integer(store_id), PyRep::Integer(htech_id)]),
                PyRep::Integer(i64::from(in_tid)),
                PyRep::Integer(in_qty as i64),
            ]));
        }
        ctx.dispatch_rpc(
            &mut sess,
            "planetMgr",
            None,
            "UserUpdateNetwork",
            &[
                PyRep::Integer(JITA_PLANET_ID),
                PyRep::List(route_cmds),
            ],
            &kw,
            None,
        )
        .await
        .expect("Create input and output routes for P1, P2, and P4 schematics");

        ctx.advance_virtual_clock(3_600);
        let col_routed = ctx
            .dispatch_rpc(&mut sess, "planetMgr", None, "GetColony", &[PyRep::Integer(JITA_PLANET_ID)], &kw, None)
            .await
            .expect("GetColony with full routes");
        assert_eq!(col_routed.dict_get("simulatedProcessedQuantity"), Some(&PyRep::Integer(46)));
    }
}

pub(crate) mod pi_poco_gantry_deploy_reinforce {
    // Extended Planetary Interaction, Customs Offices, and Wallet/Item Trader parity tests (Part 2b).

    use crate::common;
    use eve_db::{
        AllianceRecord, CorporationRecord, InventoryFlag, InventoryItemRecord, IskCents, TypeId,
    };
    use eve_machonet::SessionState;
    use eve_proto::PyRep;
    use eve_server::router::ServerContext;
    use eve_server::services::station_structure::structure_lifecycle as life;
    use eve_server::services::station_structure::structure_upwell_ops::StructureExtendedState;
    use std::collections::{HashMap, HashSet};

    const JITA_SOLAR_SYSTEM_ID: u32 = 30_000_142;
    const JITA_PLANET_ID: i64 = 40_009_082; // Jita IV (Gas)
    const LOWSEC_SOLAR_SYSTEM_ID: u32 = 30_000_012;
    const LOWSEC_STATION_ID: i64 = 60_012_292;
    const LOWSEC_PLANET_ID: i64 = 40_000_663;

    async fn setup_pilot(ctx: &ServerContext, session_id: u64, username: &str, password: &str, char_id: i64) -> SessionState {
        let kw = HashMap::new();
        let mut sess = SessionState::new(session_id);
        common::login_pilot(ctx, &mut sess, username, password).await;
        ctx.dispatch_rpc(&mut sess, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(char_id)], &kw, None).await.expect("SelectCharacterID");
        sess
    }

    fn seed_item(snap: &mut eve_db::WorldStateSnapshot, type_id: TypeId, owner_id: i64, location_id: i64, flag: InventoryFlag, quantity: i32, singleton: bool, is_blueprint_copy: bool, runs: Option<i32>) -> i64 {
        snap.next_entity_id += 1;
        let item_id = snap.next_entity_id;
        let mut dynamic_attributes = HashMap::new();
        if is_blueprint_copy { dynamic_attributes.insert(1115, 1.0); }
        if let Some(r) = runs { dynamic_attributes.insert(1124, r as f64); }
        snap.items.insert(item_id, InventoryItemRecord {
            item_id, type_id, owner_id, location_id, flag, quantity, singleton, version: 1,
            dynamic_attributes, source_mutaplasmid_id: None, insured_payout_isk: 0.0, is_blueprint_copy, blueprint_runs_remaining: runs,
        });
        item_id
    }

    #[tokio::test]
    async fn test_pi_poco_gantry_deploy_reinforce_tax_rates_and_standings() {
        // parity: feat:pi.customs-office-gantry-deploy-reinforce, feat:pi.customs-office-tax-and-standings, feat:pi.poco-customs-office-tax-rates
        eve_testkit::attest(&[
            "feat:pi.customs-office-gantry-deploy-reinforce",
            "feat:pi.customs-office-tax-and-standings",
            "feat:pi.poco-customs-office-tax-rates",
        ]);
        let ctx = ServerContext::new();
        crate::common::pin_clock(&ctx);
        let kw = HashMap::new();
        let owner_char = 90_000_001;
        let trader_char = 90_000_002;
        let owner_corp = 98_000_721;
        let ally_corp = 98_000_722;
        let ext_corp = 98_000_723;
        let alliance_id = 99_000_721;

        let mut s_owner = setup_pilot(&ctx, 707, "demo", "demo-secret", owner_char).await;
        let mut s_trader = setup_pilot(&ctx, 708, "other", "other-secret", trader_char).await;

        let owner_ship = s_owner.ship_id.expect("Owner ship");
        let (gantry_hs_id, gantry_ls_id, poco_upgrade_id) = ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&owner_char) {
                ch.corporation_id = owner_corp;
                ch.alliance_id = Some(alliance_id);
                ch.station_id = None;
                ch.solar_system_id = JITA_SOLAR_SYSTEM_ID;
            }
            if let Some(ch) = snap.characters.get_mut(&trader_char) {
                ch.corporation_id = ally_corp;
                ch.alliance_id = Some(alliance_id);
                ch.station_id = None;
                ch.solar_system_id = LOWSEC_SOLAR_SYSTEM_ID;
                ch.balance = IskCents::from_isk(100_000_000);
            }
            snap.alliances.insert(
                alliance_id,
                AllianceRecord {
                    alliance_id,
                    name: "Orbital Alliance".into(),
                    short_name: "ORBA".into(),
                    executor_corp_id: owner_corp,
                    member_corp_ids: HashSet::from([owner_corp, ally_corp]),
                },
            );
            for (cid, ceo, al) in [
                (owner_corp, owner_char, Some(alliance_id)),
                (ally_corp, trader_char, Some(alliance_id)),
                (ext_corp, trader_char, None),
            ] {
                snap.corporations.insert(
                    cid,
                    CorporationRecord {
                        corporation_id: cid,
                        name: format!("Corp {cid}"),
                        ticker: "CRP".into(),
                        ceo_id: ceo,
                        alliance_id: al,
                        tax_rate: 0.05,
                        master_wallet: IskCents::from_isk(10_000_000),
                        members: HashSet::from([ceo]),
                        member_roles: HashMap::from([(ceo, u64::MAX)]),
                        applications: HashMap::new(),
                        wallet_divisions: Vec::new(),
                        description: String::new(),
                        url: String::new(),
                        hq_station_id: Some(LOWSEC_STATION_ID),
                    },
                );
            }
            (
                seed_item(snap, 3962, owner_char, owner_ship, InventoryFlag::Cargo, 1, false, false, None),
                seed_item(snap, 3962, owner_char, owner_ship, InventoryFlag::Cargo, 1, false, false, None),
                seed_item(snap, 2233, owner_char, owner_ship, InventoryFlag::Cargo, 1, false, false, None),
            )
        });

        // 1. Customs Office Gantry deploy & upgrade to POCO
        s_owner.station_id = None;
        s_owner.corporation_id = Some(owner_corp);
        s_owner.corp_role = eve_db::corp_roles::ROLE_DIRECTOR;
        s_owner.solar_system_id = Some(JITA_SOLAR_SYSTEM_ID);
        let hs_err = ctx
            .dispatch_rpc(&mut s_owner, "posMgr", None, "AnchorOrbital", &[PyRep::Integer(gantry_hs_id), PyRep::Integer(JITA_PLANET_ID)], &kw, None)
            .await
            .expect_err("Anchoring Gantry in high-sec must fail");
        assert_eq!(hs_err.code, "CannotAnchorOrbitalInHighSec");

        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&owner_char) {
                ch.solar_system_id = LOWSEC_SOLAR_SYSTEM_ID;
            }
        });
        s_owner.solar_system_id = Some(LOWSEC_SOLAR_SYSTEM_ID);
        let ls_planet = ctx
            .sde
            .celestials_in_system(LOWSEC_SOLAR_SYSTEM_ID)
            .into_iter()
            .find(|c| c.celestial_id == LOWSEC_PLANET_ID)
            .expect("lowsec planet");
        let ls_sys = ctx.systems.get_or_spawn(LOWSEC_SOLAR_SYSTEM_ID);
        ls_sys
            .spawn_ball(eve_destiny::Ball {
                id: owner_ship,
                type_id: 587,
                position: ls_planet.position + glam::DVec3::new(ls_planet.radius + 5_000.0, 0.0, 0.0),
                velocity: glam::DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: eve_destiny::MovementMode::Stop,
            })
            .await
            .expect("spawn owner_ship near lowsec planet");
        let gantry_res = ctx
            .dispatch_rpc(&mut s_owner, "posMgr", None, "AnchorOrbital", &[PyRep::Integer(gantry_ls_id), PyRep::Integer(LOWSEC_PLANET_ID)], &kw, None)
            .await
            .expect("Anchor Customs Office Gantry at low-sec planet");
        let orbital_id = gantry_res.dict_get("orbitalID").and_then(|v| v.as_i64()).unwrap();
        assert_eq!(gantry_res.dict_get("typeID"), Some(&PyRep::Integer(3962)));

        let online_res = ctx
            .dispatch_rpc(&mut s_owner, "posMgr", None, "OnlineOrbital", &[PyRep::Integer(orbital_id), PyRep::Integer(LOWSEC_PLANET_ID)], &kw, None)
            .await
            .expect("Upgrade Gantry to POCO via OnlineOrbital");
        assert_eq!(online_res.dict_get("typeID"), Some(&PyRep::Integer(2233)));
        assert!(ctx.db.get_item(poco_upgrade_id).is_none());

        // 2. Configure POCO tax rates for Corp, Alliance, and Standing brackets (+10, +5, 0, -5, -10) and standing threshold
        ctx.dispatch_rpc(
            &mut s_owner,
            "planetOrbitalRegistryBroker",
            None,
            "UpdateSettings",
            &[
                PyRep::Integer(LOWSEC_PLANET_ID),
                PyRep::Integer(18),
                PyRep::keyval([
                    ("corporation", PyRep::Real(0.02)),
                    ("alliance", PyRep::Real(0.04)),
                    ("standingHigh", PyRep::Real(0.05)),
                    ("standingGood", PyRep::Real(0.08)),
                    ("standingNeutral", PyRep::Real(0.12)),
                    ("standingBad", PyRep::Real(0.18)),
                    ("standingHorrible", PyRep::Real(0.25)),
                ]),
                PyRep::Real(-5.0),
                PyRep::Bool(true),
                PyRep::Bool(true),
            ],
            &kw,
            None,
        )
        .await
        .expect("UpdateSettings on POCO");

        assert_eq!(
            ctx.dispatch_rpc(&mut s_owner, "planetOrbitalRegistryBroker", None, "GetTaxRate", &[PyRep::Integer(LOWSEC_PLANET_ID)], &kw, None).await.unwrap(),
            PyRep::Real(0.02)
        );
        assert_eq!(
            ctx.dispatch_rpc(&mut s_trader, "planetOrbitalRegistryBroker", None, "GetTaxRate", &[PyRep::Integer(LOWSEC_PLANET_ID)], &kw, None).await.unwrap(),
            PyRep::Real(0.04)
        );

        for (standing_val, expected_rate) in [
            (8.0, Some(0.05)),
            (3.0, Some(0.08)),
            (0.0, Some(0.12)),
            (-5.0, Some(0.18)),
            (-8.0, None),
        ] {
            ctx.db.with_mut_snapshot(|snap| {
                if let Some(ch) = snap.characters.get_mut(&trader_char) {
                    ch.corporation_id = ext_corp;
                    ch.alliance_id = None;
                    ch.standings.insert(owner_corp, standing_val);
                }
            });
            let rate_res = ctx
                .dispatch_rpc(&mut s_trader, "planetOrbitalRegistryBroker", None, "GetTaxRate", &[PyRep::Integer(LOWSEC_PLANET_ID)], &kw, None)
                .await;
            match expected_rate {
                Some(r) => assert_eq!(rate_res.unwrap(), PyRep::Real(r)),
                None => assert_eq!(rate_res.unwrap_err().code, "CustomsOfficeAccessDenied"),
            }
        }

        // 3. POCO export/import tax formula across P0..P4 tiers
        s_trader.solar_system_id = Some(LOWSEC_SOLAR_SYSTEM_ID);
        s_trader.station_id = None;
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&trader_char) {
                ch.standings.insert(owner_corp, 0.0);
            }
            snap.planetary_colonies.insert(
                (trader_char, LOWSEC_PLANET_ID),
                eve_db::PlanetaryColonyRecord {
                    owner_id: trader_char,
                    planet_id: LOWSEC_PLANET_ID,
                    solar_system_id: LOWSEC_SOLAR_SYSTEM_ID,
                    command_center_level: 5,
                    pins_count: 2,
                    extracted_commodities: HashMap::from([
                        (2268, 100),
                        (3645, 10),
                        (2329, 10),
                        (2345, 5),
                        (2867, 2),
                    ]),
                    last_extracted_unix: 0,
                },
            );
        });
        ctx.dispatch_rpc(
            &mut s_owner,
            "planetOrbitalRegistryBroker",
            None,
            "UpdateSettings",
            &[
                PyRep::Integer(LOWSEC_PLANET_ID),
                PyRep::Integer(18),
                PyRep::keyval([("standingNeutral", PyRep::Real(0.10))]),
                PyRep::Real(-10.0),
                PyRep::Bool(true),
                PyRep::Bool(true),
            ],
            &kw,
            None,
        )
        .await
        .expect("Set neutral tax rate to 10%");

        let corp_bal_before = ctx.db.with_snapshot(|snap| snap.corporations[&owner_corp].master_wallet);
        for (tid, base_val) in [
            (2268_u32, 5.0_f64),
            (3645, 400.0),
            (2329, 7_200.0),
            (2345, 60_000.0),
            (2867, 1_200_000.0),
        ] {
            let exp = ctx
                .dispatch_rpc(
                    &mut s_trader,
                    "planetMgr",
                    None,
                    "UserLaunchCommodities",
                    &[PyRep::Integer(LOWSEC_PLANET_ID), PyRep::Integer(tid as i64), PyRep::Integer(1), PyRep::String("CustomsOffice".into())],
                    &kw,
                    None,
                )
                .await
                .expect("Export tier commodity");
            let exp_tax = exp.dict_get("exportTaxISK").and_then(|v| v.as_f64()).unwrap();
            assert!((exp_tax - base_val * 0.10).abs() < 1e-4);

            let imp = ctx
                .dispatch_rpc(
                    &mut s_trader,
                    "planetMgr",
                    None,
                    "UserLaunchCommodities",
                    &[PyRep::Integer(LOWSEC_PLANET_ID), PyRep::Integer(tid as i64), PyRep::Integer(1), PyRep::String("ImportToColony".into())],
                    &kw,
                    None,
                )
                .await
                .expect("Import tier commodity");
            let imp_tax = imp.dict_get("importTaxISK").and_then(|v| v.as_f64()).unwrap();
            assert!((imp_tax - 0.5 * base_val * 0.10).abs() < 1e-4);
        }
        let corp_bal_after = ctx.db.with_snapshot(|snap| snap.corporations[&owner_corp].master_wallet);
        assert!(corp_bal_after.0 > corp_bal_before.0);

        // 4. Reinforce and destroy POCO via a single shield reinforcement timer
        let mut ext = StructureExtendedState::default();
        let now_u = ctx.now_unix();
        let dmg_1 = ctx.db.with_mut_snapshot(|snap| {
            life::apply_structure_damage(&ctx, snap, &mut ext, orbital_id, 50_000_000.0, 5.0, owner_char, now_u)
        }).expect("Apply shield damage to POCO");
        assert_eq!(dmg_1.state, life::STATE_ARMOR_REINFORCED);

        let rf_until = ctx.db.with_snapshot(|snap| snap.structures[&orbital_id].reinforced_until_unix);
        let delta = (rf_until + 10).saturating_sub(ctx.now_unix());
        ctx.advance_virtual_clock(delta);
        let now_u2 = ctx.now_unix();
        ctx.db.with_mut_snapshot(|snap| {
            life::sync_structure(&ctx, snap, &mut ext, orbital_id, now_u2);
        });
        let dmg_2 = ctx.db.with_mut_snapshot(|snap| {
            life::apply_structure_damage(&ctx, snap, &mut ext, orbital_id, 50_000_000.0, 5.0, owner_char, now_u2)
        }).expect("Apply armor damage to POCO");
        assert_eq!(dmg_2.state, life::STATE_HULL_VULNERABLE);

        let dmg_3 = ctx.db.with_mut_snapshot(|snap| {
            life::apply_structure_damage(&ctx, snap, &mut ext, orbital_id, 50_000_000.0, 5.0, owner_char, now_u2)
        }).expect("Destroy POCO after single reinforcement timer");
        assert_eq!(dmg_3.state, life::STATE_DESTROYED);
    }

    async fn get_lp(ctx: &ServerContext, sess: &mut SessionState, corp_id: i64) -> i64 {
        let kw = HashMap::new();
        let lps = ctx
            .dispatch_rpc(sess, "LPSvc", None, "GetAllMyCharacterWalletLPBalances", &[], &kw, None)
            .await
            .expect("GetAllMyCharacterWalletLPBalances");
        if let PyRep::List(list) = lps {
            for entry in list {
                if entry.dict_get("corpID").and_then(|v| v.as_i64()) == Some(corp_id) {
                    return entry.dict_get("loyaltyPoints").and_then(|v| v.as_i64()).unwrap_or(0);
                }
            }
        }
        0
    }

    #[tokio::test]
    async fn test_wallet_evermarks_and_item_trader() {
        // parity: feat:wallet.item-trader
        eve_testkit::attest(&["feat:wallet.item-trader"]);
        let ctx = ServerContext::new();
        crate::common::pin_clock(&ctx);
        let kw = HashMap::new();
        let char_id = 90_000_001;
        let mut sess = setup_pilot(&ctx, 709, "demo", "demo-secret", char_id).await;
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&char_id) {
                ch.station_id = Some(60_015_169);
                let ship_id = ch.active_ship_id;
                if let Some(ship) = snap.items.get_mut(&ship_id) {
                    ship.location_id = 60_015_169;
                }
            }
        });
        sess.station_id = Some(60_015_169);

        let paragon_corp_id: i64 = 1_000_419;
        let iris_agent_id: i64 = 3_020_034; // IRIS - Jita

        let em_before = get_lp(&ctx, &mut sess, paragon_corp_id).await;
        assert_eq!(em_before, 0);

        let m_req = ctx.dispatch_rpc(&mut sess, "agentMgr", None, "DoAction", &[PyRep::Integer(2), PyRep::Integer(iris_agent_id)], &kw, None)
            .await
            .expect("Request Paragon IRIS mission");
        let dest_st = m_req.dict_get("destinationStationID").and_then(|v| v.as_i64()).unwrap_or(60_015_169);
        let dest_sys = ctx.sde.stations.get(&dest_st).map(|s| s.solar_system_id).unwrap_or(JITA_SOLAR_SYSTEM_ID);
        ctx.dispatch_rpc(&mut sess, "agentMgr", None, "DoAction", &[PyRep::Integer(3), PyRep::Integer(iris_agent_id)], &kw, None)
            .await
            .expect("Accept Paragon IRIS mission");

        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&char_id) {
                ch.station_id = Some(dest_st);
                ch.solar_system_id = dest_sys;
                for it in snap.items.values_mut() {
                    if it.owner_id == char_id && it.location_id != ch.active_ship_id {
                        it.location_id = dest_st;
                        it.flag = InventoryFlag::Hangar;
                    }
                }
            }
        });
        sess.station_id = Some(dest_st);
        sess.solar_system_id = Some(dest_sys);

        let mission_done = ctx
            .dispatch_rpc(&mut sess, "agentMgr", None, "DoAction", &[PyRep::Integer(6), PyRep::Integer(iris_agent_id)], &kw, None)
            .await
            .expect("Complete Paragon IRIS mission");
        let gross_em = mission_done.dict_get("lpReward").and_then(|v| v.as_i64()).unwrap();
        // `lpReward` is the gross offer; NPC-corp members are credited net of the 11% LP tax when ISK >= 100k.
        let isk_reward = mission_done.dict_get("iskReward").and_then(|v| v.as_i64()).unwrap_or(0);
        let mission_em = if isk_reward >= 100_000 { gross_em - (gross_em * 11 + 50) / 100 } else { gross_em };
        assert!(mission_em > 0);

        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&char_id) {
                ch.station_id = Some(60_015_169);
                ch.solar_system_id = JITA_SOLAR_SYSTEM_ID;
                let ship_id = ch.active_ship_id;
                if let Some(ship) = snap.items.get_mut(&ship_id) {
                    ship.location_id = 60_015_169;
                }
            }
        });
        sess.station_id = Some(60_015_169);
        sess.solar_system_id = Some(JITA_SOLAR_SYSTEM_ID);

        use eve_proto::quasar::{encode_bytes_field, encode_string_field, encode_varint_field};
        let mk_quasar_req = |corr: &[u8], type_name: &str, payload: Vec<u8>| {
            eve_proto::quasar::envelopes::EvePublicRequest {
                issued: Some(eve_proto::quasar::types::GoogleTimestamp {
                    seconds: ctx.now_unix() as i64,
                    nanos: 0,
                }),
                correlation_uuid: corr.to_vec(),
                payload: Some(eve_proto::quasar::types::GoogleAny {
                    type_url: format!("{}{type_name}", eve_proto::quasar::TYPE_GOOGLEAPIS_PREFIX),
                    value: payload,
                }),
                external_origin: "eve-client".into(),
                application_instance_uuid: b"app-instance-001".to_vec(),
                authoritative_context: None,
                trace_context: None,
            }
        };

        // Empty SKINR InitiateRequest must NOT mint free EverMarks
        let empty_req = mk_quasar_req(
            b"corr-seq-empty",
            "eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.api.InitiateRequest",
            Vec::new(),
        );
        let empty_rsp = ctx.quasar_gateway.handle_public_request(&sess, &empty_req, None, false, ctx.now_unix());
        assert_eq!(empty_rsp.status_code, 400);
        assert_eq!(get_lp(&ctx, &mut sess, paragon_corp_id).await, mission_em);

        // Activate a real SDE SKINR coating component item & initiate validated SKINR sequencing jobs
        let skinr_cat = eve_server::services::quasar_cosmetics_skinr::SkinrStaticCatalog::load(&ctx.sde);
        let coating_comp = skinr_cat
            .components
            .values()
            .find(|c| !c.is_pattern() && !c.associated_types.is_empty() && !c.disallowed_slots.contains(&1))
            .expect("SDE SKINR coating component");
        let (coating_item_type_id, _) = coating_comp.associated_types[0];
        let coating_item_id = ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&char_id) {
                ch.balance = IskCents::from_isk(1_000_000);
            }
            seed_item(snap, coating_item_type_id, char_id, 60_015_169, InventoryFlag::Hangar, 10, false, false, None)
        });
        let mut seq_item = Vec::new();
        encode_varint_field(1, coating_item_id as u64, &mut seq_item);
        let mut item_outer = Vec::new();
        encode_bytes_field(1, &seq_item, &mut item_outer);
        let mut consume_req = Vec::new();
        encode_bytes_field(1, &item_outer, &mut consume_req);
        encode_varint_field(2, 10, &mut consume_req);
        let consume_rsp = ctx.quasar_gateway.handle_public_request(
            &sess,
            &mk_quasar_req(b"corr-consume-01", "eve_public.cosmetic.ship.skin.thirdparty.component.license.item.api.ConsumeRequest", consume_req),
            None,
            false,
            ctx.now_unix(),
        );
        assert_eq!(consume_rsp.status_code, 200, "Consume coating item: {}", consume_rsp.status_message);

        let build_skinr_init = |design_name: &str, qty: u64| {
            let mut seq_comp = Vec::new();
            encode_varint_field(1, coating_comp.component_id as u64, &mut seq_comp);
            let mut coat_cfg = Vec::new();
            encode_bytes_field(1, &seq_comp, &mut coat_cfg);
            let mut cfg_outer = Vec::new();
            encode_bytes_field(1, &coat_cfg, &mut cfg_outer);
            let mut seq_slot = Vec::new();
            encode_varint_field(1, 1, &mut seq_slot);
            let mut slot_msg = Vec::new();
            encode_bytes_field(1, &seq_slot, &mut slot_msg);
            encode_bytes_field(2, &cfg_outer, &mut slot_msg);
            let mut seq_hull = Vec::new();
            encode_varint_field(1, 587, &mut seq_hull); // Rifter
            let mut layout = Vec::new();
            encode_bytes_field(1, &slot_msg, &mut layout);
            encode_bytes_field(2, &seq_hull, &mut layout);
            encode_varint_field(3, 1, &mut layout);
            let mut init_req = Vec::new();
            encode_string_field(1, design_name, &mut init_req);
            encode_string_field(2, "Paragon Line", &mut init_req);
            encode_bytes_field(3, &layout, &mut init_req);
            encode_varint_field(5, qty, &mut init_req);
            init_req
        };

        let rsp1 = ctx.quasar_gateway.handle_public_request(
            &sess,
            &mk_quasar_req(b"corr-seq-job01", "eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.api.InitiateRequest", build_skinr_init("Valkyrie I", 1)),
            None,
            false,
            ctx.now_unix(),
        );
        assert_eq!(rsp1.status_code, 200, "SKINR sequencing job status: {}", rsp1.status_message);
        assert_eq!(get_lp(&ctx, &mut sess, paragon_corp_id).await, mission_em);

        let rsp2 = ctx.quasar_gateway.handle_public_request(
            &sess,
            &mk_quasar_req(b"corr-seq-job02", "eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.api.InitiateRequest", build_skinr_init("Valkyrie II", 9)),
            None,
            false,
            ctx.now_unix(),
        );
        assert_eq!(rsp2.status_code, 200);
        let em_earned = get_lp(&ctx, &mut sess, paragon_corp_id).await;
        assert_eq!(em_earned, mission_em);
        let em_before_buy = 5_000;
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&char_id) {
                ch.loyalty_points.insert(paragon_corp_id, em_before_buy);
            }
        });

        // Query Paragon Tranquility ESI + V24 SDE EverMarks Ship Cosmetic Emblem catalog without synthetic seeding
        let paragon_offers = ctx
            .dispatch_rpc(&mut sess, "LPStoreMgr", None, "GetAvailableOffersFromCorp", &[PyRep::Integer(paragon_corp_id)], &kw, None)
            .await
            .expect("Paragon SDE EverMarks offers");
        let PyRep::List(offer_list) = paragon_offers else { panic!("Expected List of Paragon offers") };
        assert!(offer_list.len() > 800, "Expected >800 Paragon ship emblem offers, got {}", offer_list.len());
        let rifter_emblem_offer_id = 17_208;
        let rifter_emblem_cost = offer_list
            .iter()
            .find(|o| o.dict_get("offerID") == Some(&PyRep::Integer(rifter_emblem_offer_id)) && o.dict_get("typeID") == Some(&PyRep::Integer(72964)))
            .and_then(|o| o.dict_get("lpCost"))
            .and_then(|v| v.as_i64())
            .expect("Tranquility Rifter emblem offer 17208 in Paragon catalog");
        assert_eq!(rifter_emblem_cost, 5_000);

        let take_emblem = ctx
            .dispatch_rpc(&mut sess, "LPStoreMgr", None, "TakeOfferForCharacter", &[PyRep::Integer(paragon_corp_id), PyRep::Integer(rifter_emblem_offer_id), PyRep::Integer(1)], &kw, None)
            .await
            .expect("Redeem EverMarks for ship emblem at Paragon store");
        assert_eq!(take_emblem.dict_get("deliveredTypeID"), Some(&PyRep::Integer(72964)));
        assert_eq!(take_emblem.dict_get("remainingLP"), Some(&PyRep::Integer(em_before_buy - rifter_emblem_cost)));

        // 2. In-space Item Trader (`beyonce.CallComponentFromClient` -> `itemTrader.ProcessTrade`)
        ctx.dispatch_rpc(&mut sess, "stationSvc", None, "Undock", &[], &kw, None)
            .await
            .expect("Undock into Jita space");
        let ship_id = sess.ship_id.expect("Active ship in space");
        let sys_handle = ctx.systems.get_or_spawn(JITA_SOLAR_SYSTEM_ID);
        assert!(sys_handle.set_ball_position(ship_id, glam::DVec3::ZERO).await);

        let trader_entity_id = ctx.db.with_mut_snapshot(|snap| {
            let tid = seed_item(snap, 86278, 1_000_125, JITA_SOLAR_SYSTEM_ID as i64, InventoryFlag::Hangar, 1, true, false, None);
            snap.item_positions.insert(tid, [40_000.0, 0.0, 0.0]);
            tid
        });

        let range_err = ctx
            .dispatch_rpc(&mut sess, "beyonce", None, "CallComponentFromClient", &[PyRep::Integer(trader_entity_id), PyRep::String("itemTrader".into()), PyRep::String("ProcessTrade".into()), PyRep::Integer(67), PyRep::Integer(1)], &kw, None)
            .await
            .expect_err("Item trader outside interactionRange must fail");
        assert_eq!(range_err.code, "ItemTraderOutOfRange");

        assert!(sys_handle.set_ball_position(ship_id, glam::DVec3::new(15_000.0, 0.0, 0.0)).await);

        let bad_recipe_err = ctx
            .dispatch_rpc(&mut sess, "beyonce", None, "CallComponentFromClient", &[PyRep::Integer(trader_entity_id), PyRep::String("itemTrader".into()), PyRep::String("ProcessTrade".into()), PyRep::Integer(1), PyRep::Integer(1)], &kw, None)
            .await
            .expect_err("Recipe not offered by trader must fail");
        assert_eq!(bad_recipe_err.code, "RecipeNotAvailableAtTrader");

        let missing_in_err = ctx
            .dispatch_rpc(&mut sess, "beyonce", None, "CallComponentFromClient", &[PyRep::Integer(trader_entity_id), PyRep::String("itemTrader".into()), PyRep::String("ProcessTrade".into()), PyRep::Integer(67), PyRep::Integer(1)], &kw, None)
            .await
            .expect_err("Missing input items must fail");
        assert_eq!(missing_in_err.code, "InsufficientInputItems");

        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&char_id) {
                ch.balance = IskCents::from_isk(50_000_000);
            }
            seed_item(snap, 85316, char_id, ship_id, InventoryFlag::Cargo, 30, false, false, None);
        });
        let bal_before = ctx.db.get_character(char_id).unwrap().balance;
        let trade_ok = ctx
            .dispatch_rpc(&mut sess, "beyonce", None, "CallComponentFromClient", &[PyRep::Integer(trader_entity_id), PyRep::String("itemTrader".into()), PyRep::String("ProcessTrade".into()), PyRep::Integer(67), PyRep::Integer(2)], &kw, None)
            .await
            .expect("Execute 2x recipe 67 at Item Trader");
        assert!(matches!(trade_ok, PyRep::Tuple(ref items) if items.first() == Some(&PyRep::Bool(true))));

        let bal_after = ctx.db.get_character(char_id).unwrap().balance;
        assert_eq!(bal_before.0 - bal_after.0, 0);
        let (rem_in, out_qty) = ctx.db.with_snapshot(|snap| {
            let r_in: i32 = snap
                .items
                .values()
                .filter(|i| i.owner_id == char_id && i.location_id == ship_id && i.type_id == 85316)
                .map(|i| i.quantity)
                .sum();
            let r_out: i32 = snap
                .items
                .values()
                .filter(|i| i.owner_id == char_id && i.location_id == ship_id && i.type_id == 86272)
                .map(|i| i.quantity)
                .sum();
            (r_in, r_out)
        });
        assert_eq!(rem_in, 0);
        assert_eq!(out_qty, 2);
    }
}

pub(crate) mod industry_eiv_scc_surcharge_alpha {
    // Extended Industry, Planetary Interaction, and Wallet/Item Trader parity tests (Part 3).

    use crate::common;
    use eve_db::{
        InventoryFlag, InventoryItemRecord, IskCents, TrainedSkillRecord, TypeId, UpwellStructureRecord,
    };
    use eve_machonet::SessionState;
    use eve_proto::PyRep;
    use eve_server::rng::MechanicRng;
    use eve_server::router::ServerContext;
    use std::collections::HashMap;

    const JITA_SOLAR_SYSTEM_ID: u32 = 30_000_142;
    const JITA_STATION_ID: i64 = 60_003_760;
    const ZARZAKH_SOLAR_SYSTEM_ID: u32 = 30_100_000;
    const FULCRUM_FACILITY_ID: i64 = 60_015_187;

    const RIFTER_BP_TYPE_ID: TypeId = 691;
    const SLASH_BP_TYPE_ID: TypeId = 693;

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
            last_damage_unix: 0,
            self_destruct_at_unix: None,
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
    async fn test_industry_eiv_scc_surcharge_alpha_tax_and_fulcrum_pirate_bonus() {
        // parity: feat:industry.scc-surcharge-alpha-tax, feat:industry.fulcrum-pirate-ship-bonus
        eve_testkit::attest(&[
            "feat:industry.scc-surcharge-alpha-tax",
            "feat:industry.fulcrum-pirate-ship-bonus",
        ]);
        let ctx = ServerContext::new();
        crate::common::pin_clock(&ctx);
        let kw = HashMap::new();
        let char_id = 90_000_001;
        let mut sess = setup_pilot(&ctx, 703, "demo", "demo-secret", char_id).await;

        let rifter_mats = ctx.sde.blueprints[&RIFTER_BP_TYPE_ID].activities["manufacturing"].materials.clone();
        let expected_eiv_1_run = eve_server::services::industry_pi::compute_blueprint_eiv_per_run(
            &ctx,
            &ctx.sde.blueprints[&RIFTER_BP_TYPE_ID],
            1,
        );

        let (bpo_omega, bpo_alpha, dramiel_bp_std, dramiel_bp_fulcrum) = ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&char_id) {
                ch.balance = IskCents::from_isk(500_000_000);
                ch.clone_grade = 2; // Omega
                ch.war_faction_id = Some(500_011); // Angel Cartel enlistment for Fulcrum bonus
            }
            let sk = snap.skills.entry(char_id).or_default();
            for (stid, lvl) in [(3380, 5), (3387, 5), (33092, 5), (33093, 5)] {
                sk.insert(stid, TrainedSkillRecord { type_id: stid, level: lvl, skill_points: 256_000.0 });
            }
            for sk_req in &ctx.sde.blueprints[&17933].activities["manufacturing"].skills {
                sk.insert(sk_req.type_id, TrainedSkillRecord { type_id: sk_req.type_id, level: 5, skill_points: 256_000.0 });
            }
            for loc in [JITA_STATION_ID, FULCRUM_FACILITY_ID] {
                for m in &rifter_mats {
                    seed_item(snap, m.type_id, char_id, loc, InventoryFlag::Hangar, (m.quantity * 20) as i32, false, false, None);
                }
                for m in &ctx.sde.blueprints[&17933].activities["manufacturing"].materials {
                    seed_item(snap, m.type_id, char_id, loc, InventoryFlag::Hangar, (m.quantity * 20) as i32, false, false, None);
                }
            }
            (
                seed_item(snap, RIFTER_BP_TYPE_ID, char_id, JITA_STATION_ID, InventoryFlag::Hangar, 1, true, false, None),
                seed_item(snap, RIFTER_BP_TYPE_ID, char_id, JITA_STATION_ID, InventoryFlag::Hangar, 1, true, false, None),
                seed_item(snap, 17933, char_id, JITA_STATION_ID, InventoryFlag::Hangar, 1, true, false, None),
                seed_item(snap, 17933, char_id, FULCRUM_FACILITY_ID, InventoryFlag::Hangar, 1, true, false, None),
            )
        });

        let omega_job = ctx
            .dispatch_rpc(
                &mut sess,
                "industryManager",
                None,
                "InstallJob",
                &[PyRep::keyval([
                    ("blueprintID", PyRep::Integer(bpo_omega)),
                    ("facilityID", PyRep::Integer(JITA_STATION_ID)),
                    ("activityID", PyRep::Integer(1)),
                    ("runs", PyRep::Integer(2)),
                ])],
                &kw,
                None,
            )
            .await
            .expect("Install Omega manufacturing job");
        let eiv_reported = omega_job.dict_get("estimatedItemValue").and_then(|v| v.as_f64()).unwrap();
        let sci_reported = omega_job.dict_get("systemCostIndex").and_then(|v| v.as_f64()).unwrap();
        let base_cost_omega = omega_job.dict_get("baseJobCost").and_then(|v| v.as_f64()).unwrap();
        let fac_tax_omega = omega_job.dict_get("facilityTaxISK").and_then(|v| v.as_f64()).unwrap();
        let scc_omega = omega_job.dict_get("sccSurchargeISK").and_then(|v| v.as_f64()).unwrap();
        let alpha_omega = omega_job.dict_get("alphaCloneTaxISK").and_then(|v| v.as_f64()).unwrap();
        let total_omega = omega_job.dict_get("totalJobCost").and_then(|v| v.as_f64()).unwrap();

        assert!((eiv_reported - expected_eiv_1_run).abs() < 1.0);
        assert!((base_cost_omega - eiv_reported * sci_reported * 2.0).abs() < 0.05);
        assert!((scc_omega - eiv_reported * 2.0 * 0.04).abs() < 0.05);
        assert_eq!(alpha_omega, 0.0);
        assert!((total_omega - (base_cost_omega + fac_tax_omega + scc_omega)).abs() < 0.05);

        // Switch pilot to Alpha clone (clone_grade = 1) -> verify 0.25% Alpha clone tax is charged and journaled
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&char_id) {
                ch.clone_grade = 1;
            }
        });
        let alpha_job = ctx
            .dispatch_rpc(
                &mut sess,
                "industryManager",
                None,
                "InstallJob",
                &[PyRep::keyval([
                    ("blueprintID", PyRep::Integer(bpo_alpha)),
                    ("facilityID", PyRep::Integer(JITA_STATION_ID)),
                    ("activityID", PyRep::Integer(1)),
                    ("runs", PyRep::Integer(2)),
                ])],
                &kw,
                None,
            )
            .await
            .expect("Install Alpha manufacturing job");
        let sci_alpha = alpha_job.dict_get("systemCostIndex").and_then(|v| v.as_f64()).unwrap();
        let base_cost_alpha = alpha_job.dict_get("baseJobCost").and_then(|v| v.as_f64()).unwrap();
        let fac_tax_alpha = alpha_job.dict_get("facilityTaxISK").and_then(|v| v.as_f64()).unwrap();
        let scc_alpha = alpha_job.dict_get("sccSurchargeISK").and_then(|v| v.as_f64()).unwrap();
        let alpha_tax = alpha_job.dict_get("alphaCloneTaxISK").and_then(|v| v.as_f64()).unwrap();
        let total_alpha = alpha_job.dict_get("totalJobCost").and_then(|v| v.as_f64()).unwrap();
        assert!((base_cost_alpha - eiv_reported * sci_alpha * 2.0).abs() < 0.05);
        assert!((alpha_tax - eiv_reported * 2.0 * 0.0025).abs() < 0.05);
        assert!((total_alpha - (base_cost_alpha + fac_tax_alpha + scc_alpha + alpha_tax)).abs() < 0.05);

        // 2. The Fulcrum in Zarzakh pirate ship manufacturing bonus (-4% material, -20% time)
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&char_id) {
                ch.clone_grade = 2;
            }
        });
        let std_pirate_job = ctx
            .dispatch_rpc(
                &mut sess,
                "industryManager",
                None,
                "InstallJob",
                &[PyRep::keyval([
                    ("blueprintID", PyRep::Integer(dramiel_bp_std)),
                    ("facilityID", PyRep::Integer(JITA_STATION_ID)),
                    ("activityID", PyRep::Integer(1)),
                    ("runs", PyRep::Integer(1)),
                ])],
                &kw,
                None,
            )
            .await
            .expect("Install Dramiel at standard station");
        let std_dur = std_pirate_job.dict_get("durationSeconds").and_then(|v| v.as_i64()).unwrap();
        let std_trit = std_pirate_job.dict_get("tritaniumRequired").and_then(|v| v.as_i64()).unwrap();

        move_pilot(&ctx, &mut sess, char_id, ZARZAKH_SOLAR_SYSTEM_ID, Some(FULCRUM_FACILITY_ID), None);
        let fulcrum_pirate_job = ctx
            .dispatch_rpc(
                &mut sess,
                "industryManager",
                None,
                "InstallJob",
                &[PyRep::keyval([
                    ("blueprintID", PyRep::Integer(dramiel_bp_fulcrum)),
                    ("facilityID", PyRep::Integer(FULCRUM_FACILITY_ID)),
                    ("activityID", PyRep::Integer(1)),
                    ("runs", PyRep::Integer(1)),
                ])],
                &kw,
                None,
            )
            .await
            .expect("Install Dramiel at The Fulcrum in Zarzakh");
        let fulcrum_dur = fulcrum_pirate_job.dict_get("durationSeconds").and_then(|v| v.as_i64()).unwrap();
        let fulcrum_trit = fulcrum_pirate_job.dict_get("tritaniumRequired").and_then(|v| v.as_i64()).unwrap();
        assert!(fulcrum_dur < std_dur && fulcrum_trit < std_trit);
    }

    #[tokio::test]
    async fn test_industry_invention_decryptors_rng_roll_pause_and_mutaplasmids() {
        // parity: feat:industry.invention-decryptor-run-and-chance-modifiers, feat:industry.invention-success-roll, feat:industry.job-pause-on-service-offline, feat:industry.mutaplasmid-stat-roll
        eve_testkit::attest(&[
            "feat:industry.invention-decryptor-run-and-chance-modifiers",
            "feat:industry.invention-success-roll",
            "feat:industry.job-pause-on-service-offline",
            "feat:industry.mutaplasmid-stat-roll",
        ]);
        let ctx = ServerContext::new();
        crate::common::pin_clock(&ctx);
        let kw = HashMap::new();
        let char_id = 90_000_001;
        let corp_id = 98_000_714;
        let mut sess = setup_pilot(&ctx, 704, "demo", "demo-secret", char_id).await;

        let raitaru_id = 1_045_000_000_704_i64;
        let (svc_id, bpc_inv_base, bpc_inv_aug, bpc_inv_fail, bpo_pause, muta_id, wdis_id, incompat_id) =
            ctx.db.with_mut_snapshot(|snap| {
                if let Some(ch) = snap.characters.get_mut(&char_id) {
                    ch.corporation_id = corp_id;
                    ch.balance = IskCents::from_isk(500_000_000);
                }
                snap.structures.insert(
                    raitaru_id,
                    make_structure(raitaru_id, 35825, corp_id, JITA_SOLAR_SYSTEM_ID, "Pause Raitaru"),
                );
                let svc = seed_item(snap, 35878, corp_id, raitaru_id, InventoryFlag::ServiceSlot0, 1, true, false, None);
                snap.items.get_mut(&svc).unwrap().dynamic_attributes.insert(2, 1.0);
                snap.module_online_states.insert(svc, true);
                let sk = snap.skills.entry(char_id).or_default();
                for (stid, lvl) in [(3380, 5), (3387, 5), (3402, 5), (3406, 5), (24624, 5), (11442, 5), (11454, 5), (21790, 5)] {
                    sk.insert(stid, TrainedSkillRecord { type_id: stid, level: lvl, skill_points: 256_000.0 });
                }
                for bp_tid in [RIFTER_BP_TYPE_ID, SLASH_BP_TYPE_ID] {
                    let inv_act = &ctx.sde.blueprints[&bp_tid].activities["invention"];
                    for sk_req in &inv_act.skills {
                        sk.insert(sk_req.type_id, TrainedSkillRecord { type_id: sk_req.type_id, level: 5, skill_points: 256_000.0 });
                    }
                }
                for bp_tid in [RIFTER_BP_TYPE_ID, SLASH_BP_TYPE_ID] {
                    for m in &ctx.sde.blueprints[&bp_tid].activities["invention"].materials {
                        seed_item(snap, m.type_id, char_id, JITA_STATION_ID, InventoryFlag::Hangar, (m.quantity * 20) as i32, false, false, None);
                    }
                }
                for m in &ctx.sde.blueprints[&RIFTER_BP_TYPE_ID].activities["manufacturing"].materials {
                    seed_item(snap, m.type_id, char_id, raitaru_id, InventoryFlag::Hangar, (m.quantity * 10) as i32, false, false, None);
                }
                seed_item(snap, 34203, char_id, JITA_STATION_ID, InventoryFlag::Hangar, 2, false, false, None);
                (
                    svc,
                    seed_item(snap, RIFTER_BP_TYPE_ID, char_id, JITA_STATION_ID, InventoryFlag::Hangar, 1, true, true, Some(3)),
                    seed_item(snap, RIFTER_BP_TYPE_ID, char_id, JITA_STATION_ID, InventoryFlag::Hangar, 1, true, true, Some(2)),
                    seed_item(snap, SLASH_BP_TYPE_ID, char_id, JITA_STATION_ID, InventoryFlag::Hangar, 1, true, true, Some(2)),
                    seed_item(snap, RIFTER_BP_TYPE_ID, char_id, raitaru_id, InventoryFlag::Hangar, 1, true, false, None),
                    seed_item(snap, 47735, char_id, JITA_STATION_ID, InventoryFlag::Hangar, 2, false, false, None),
                    seed_item(snap, 3244, char_id, JITA_STATION_ID, InventoryFlag::Hangar, 1, true, false, None),
                    seed_item(snap, 34, char_id, JITA_STATION_ID, InventoryFlag::Hangar, 1, false, false, None),
                )
            });

        let inv_base = ctx
            .dispatch_rpc(
                &mut sess,
                "industryManager",
                None,
                "InstallJob",
                &[PyRep::keyval([
                    ("blueprintID", PyRep::Integer(bpc_inv_base)),
                    ("facilityID", PyRep::Integer(JITA_STATION_ID)),
                    ("activityID", PyRep::Integer(8)),
                    ("runs", PyRep::Integer(1)),
                ])],
                &kw,
                None,
            )
            .await
            .expect("Install base invention job");
        assert_eq!(ctx.db.get_item(bpc_inv_base).unwrap().flag, InventoryFlag::Locked);

        let inv_aug = ctx
            .dispatch_rpc(
                &mut sess,
                "industryManager",
                None,
                "InstallJob",
                &[PyRep::keyval([
                    ("blueprintID", PyRep::Integer(bpc_inv_aug)),
                    ("facilityID", PyRep::Integer(JITA_STATION_ID)),
                    ("activityID", PyRep::Integer(8)),
                    ("runs", PyRep::Integer(1)),
                    ("decryptorTypeID", PyRep::Integer(34203)),
                ])],
                &kw,
                None,
            )
            .await
            .expect("Install Augmentation Decryptor invention job");
        let prob_base = inv_base.dict_get("probability").and_then(|v| v.as_f64()).unwrap();
        let prob_aug = inv_aug.dict_get("probability").and_then(|v| v.as_f64()).unwrap();
        assert!((prob_aug - prob_base * 0.6).abs() < 1e-4);

        let inv_fail = ctx
            .dispatch_rpc(
                &mut sess,
                "industryManager",
                None,
                "InstallJob",
                &[PyRep::keyval([
                    ("blueprintID", PyRep::Integer(bpc_inv_fail)),
                    ("facilityID", PyRep::Integer(JITA_STATION_ID)),
                    ("activityID", PyRep::Integer(8)),
                    ("runs", PyRep::Integer(1)),
                ])],
                &kw,
                None,
            )
            .await
            .expect("Install third invention job");

        ctx.advance_virtual_clock(500_000);
        let win_seed = (1u64..10_000)
            .find(|&s| MechanicRng::seeded(s).try_unit_f64().unwrap() < 0.05)
            .unwrap();
        let lose_seed = (1u64..10_000)
            .find(|&s| MechanicRng::seeded(s).try_unit_f64().unwrap() > 0.99)
            .unwrap();

        ctx.set_mechanic_rng_seed(Some(win_seed));
        let done_base = ctx
            .dispatch_rpc(
                &mut sess,
                "industryManager",
                None,
                "CompleteJob",
                &[PyRep::Integer(inv_base.dict_get("jobID").and_then(|v| v.as_i64()).unwrap())],
                &kw,
                None,
            )
            .await
            .expect("Complete base invention job");
        let t2_base_id = done_base.dict_get("createdBpcItemID").and_then(|v| v.as_i64()).unwrap();
        let t2_base_item = ctx.db.get_item(t2_base_id).unwrap();
        assert_eq!(t2_base_item.dynamic_attributes.get(&1112).copied(), Some(2.0));
        assert_eq!(t2_base_item.dynamic_attributes.get(&1113).copied(), Some(4.0));

        ctx.set_mechanic_rng_seed(Some(win_seed));
        let done_aug = ctx
            .dispatch_rpc(
                &mut sess,
                "industryManager",
                None,
                "CompleteJob",
                &[PyRep::Integer(inv_aug.dict_get("jobID").and_then(|v| v.as_i64()).unwrap())],
                &kw,
                None,
            )
            .await
            .expect("Complete Augmentation Decryptor invention job");
        let t2_aug_id = done_aug.dict_get("createdBpcItemID").and_then(|v| v.as_i64()).unwrap();
        let t2_aug_item = ctx.db.get_item(t2_aug_id).unwrap();
        assert_eq!(
            t2_aug_item.blueprint_runs_remaining,
            Some(t2_base_item.blueprint_runs_remaining.unwrap() + 9)
        );
        assert_eq!(t2_aug_item.dynamic_attributes.get(&1112).copied(), Some(0.0));
        assert_eq!(t2_aug_item.dynamic_attributes.get(&1113).copied(), Some(6.0));

        ctx.set_mechanic_rng_seed(Some(lose_seed));
        let done_fail = ctx
            .dispatch_rpc(
                &mut sess,
                "industryManager",
                None,
                "CompleteJob",
                &[PyRep::Integer(inv_fail.dict_get("jobID").and_then(|v| v.as_i64()).unwrap())],
                &kw,
                None,
            )
            .await
            .expect_err("Failed invention roll returns InventionAttemptFailed");
        assert_eq!(done_fail.code, "InventionAttemptFailed");

        // 3. Job pause on structure service offline
        let pause_job = ctx
            .dispatch_rpc(
                &mut sess,
                "industryManager",
                None,
                "InstallJob",
                &[PyRep::keyval([
                    ("blueprintID", PyRep::Integer(bpo_pause)),
                    ("facilityID", PyRep::Integer(raitaru_id)),
                    ("activityID", PyRep::Integer(1)),
                    ("runs", PyRep::Integer(1)),
                ])],
                &kw,
                None,
            )
            .await
            .expect("Install job at Raitaru");
        let pause_jid = pause_job.dict_get("jobID").and_then(|v| v.as_i64()).unwrap();

        let pause_now = ctx.now_unix();
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(st) = snap.structures.get_mut(&raitaru_id) {
                st.fuel_blocks = 0;
            }
            eve_server::services::industry_pi::industry_facilities_bps::sync_industry_job_pause_states_on_service_change(
                snap,
                pause_now,
            );
        });
        ctx.advance_virtual_clock(20_000);
        let complete_paused_err = ctx
            .dispatch_rpc(&mut sess, "industryManager", None, "CompleteJob", &[PyRep::Integer(pause_jid)], &kw, None)
            .await
            .expect_err("Paused job cannot be delivered while structure service is offline");
        assert_eq!(complete_paused_err.code, "FacilityServiceOffline");

        let resume_now = ctx.now_unix();
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(st) = snap.structures.get_mut(&raitaru_id) {
                st.fuel_blocks = 1_000;
            }
            snap.module_online_states.insert(svc_id, true);
            eve_server::services::industry_pi::industry_facilities_bps::sync_industry_job_pause_states_on_service_change(
                snap,
                resume_now,
            );
        });
        ctx.advance_virtual_clock(20_000);
        ctx.dispatch_rpc(&mut sess, "industryManager", None, "CompleteJob", &[PyRep::Integer(pause_jid)], &kw, None)
            .await
            .expect("Re-onlined job completes");

        // 4. Mutaplasmid stat roll
        let incompat_err = ctx
            .dispatch_rpc(
                &mut sess,
                "dynamicItemService",
                None,
                "CreateDynamicItem",
                &[PyRep::Integer(muta_id), PyRep::Integer(incompat_id)],
                &kw,
                None,
            )
            .await
            .expect_err("Incompatible source item must fail mutation");
        assert!(matches!(incompat_err.code.as_str(), "IncompatibleMutaplasmidSource" | "InapplicableMutaplasmid"));

        ctx.set_mechanic_rng_seed(Some(42));
        let dyn_res = ctx
            .dispatch_rpc(
                &mut sess,
                "dynamicItemService",
                None,
                "CreateDynamicItem",
                &[PyRep::Integer(muta_id), PyRep::Integer(wdis_id)],
                &kw,
                None,
            )
            .await
            .expect("Mutate Warp Disruptor II with Gravid Mutaplasmid");
        let dyn_item_id = dyn_res.dict_get("itemID").and_then(|v| v.as_i64()).unwrap();
        let dyn_item = ctx.db.get_item(dyn_item_id).expect("Dynamic item created");
        assert_eq!(dyn_item.type_id, 47736);
        assert!(dyn_item.singleton);
        assert_eq!(dyn_item.source_mutaplasmid_id, Some(47735));
        assert!(dyn_item.dynamic_attributes.len() >= 4);
    }
}
