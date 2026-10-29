//! Extended Industry, Planetary Interaction, and Wallet/Item Trader parity tests (Part 3).

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
    // parity: feat:industry.eiv-adjusted-prices, feat:industry.scc-surcharge-alpha-tax, feat:industry.fulcrum-pirate-ship-bonus
    eve_testkit::attest(&[
        "feat:industry.eiv-adjusted-prices",
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
    let (bpc_inv_base, bpc_inv_aug, bpc_inv_fail, bpo_pause, muta_id, wdis_id, incompat_id) =
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
            let sk = snap.skills.entry(char_id).or_default();
            for (stid, lvl) in [(3380, 5), (3387, 5), (3402, 5), (3406, 5), (24624, 5), (11442, 5), (11454, 5), (21790, 5)] {
                sk.insert(stid, TrainedSkillRecord { type_id: stid, level: lvl, skill_points: 256_000.0 });
            }
            let inv_act = &ctx.sde.blueprints[&RIFTER_BP_TYPE_ID].activities["invention"];
            for sk_req in &inv_act.skills {
                sk.insert(sk_req.type_id, TrainedSkillRecord { type_id: sk_req.type_id, level: 5, skill_points: 256_000.0 });
            }
            for m in &inv_act.materials {
                seed_item(snap, m.type_id, char_id, JITA_STATION_ID, InventoryFlag::Hangar, (m.quantity * 20) as i32, false, false, None);
            }
            for m in &ctx.sde.blueprints[&RIFTER_BP_TYPE_ID].activities["manufacturing"].materials {
                seed_item(snap, m.type_id, char_id, raitaru_id, InventoryFlag::Hangar, (m.quantity * 10) as i32, false, false, None);
            }
            seed_item(snap, 34203, char_id, JITA_STATION_ID, InventoryFlag::Hangar, 2, false, false, None);
            (
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

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(st) = snap.structures.get_mut(&raitaru_id) {
            st.fuel_blocks = 0;
        }
    });
    ctx.advance_virtual_clock(20_000);
    let complete_paused_err = ctx
        .dispatch_rpc(&mut sess, "industryManager", None, "CompleteJob", &[PyRep::Integer(pause_jid)], &kw, None)
        .await
        .expect_err("Paused job cannot be delivered while structure service is offline");
    assert_eq!(complete_paused_err.code, "FacilityServiceOffline");

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(st) = snap.structures.get_mut(&raitaru_id) {
            st.fuel_blocks = 1_000;
        }
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
