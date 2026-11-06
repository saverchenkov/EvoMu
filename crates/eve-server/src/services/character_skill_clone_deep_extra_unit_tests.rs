//! Deep branch-coverage and mutation-killing unit tests for `character_skills`,
//! `character_clones_implants`, `character_clone_jump_rules`, `character_contacts`,
//! and `implant_plugging`.

#![forbid(unsafe_code)]

use crate::formulas::{skill_injector_sp_yield, sp_for_skill_level};
use crate::router::ServerContext;
use crate::services::character_skills::{self, catch_up_skill_queue};
use crate::services::implant_plugging::{
    apply_active_booster_side_effect_adjustments, handle_skill_notifications_and_injections,
    purge_expired_boosters_in_snapshot, sync_clone_jump_implants_and_boosters, use_booster,
    ATTR_CONTAINED_SKILL_POINTS, DYN_ATTR_BOOSTER_DURATION_SECS, DYN_ATTR_BOOSTER_EXPIRY_UNIX,
    TYPE_DAILY_ALPHA_INJECTOR,
};
use eve_db::{InventoryFlag, InventoryItemRecord, IskCents, TrainedSkillRecord};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::{EffectModifierSpec, SdeRegistry};
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

const SYNTH_COMBAT_BOOSTER_TID: u32 = 990_101;
const SYNTH_CEREBRAL_ACCEL_TID: u32 = 990_102;
const SYNTH_NON_DIM_INJ_TID: u32 = 990_103;
const SYNTH_REQ_IMPLANT_TID: u32 = 990_104;
const SYNTH_SLOT1_IMPLANT_TID: u32 = 990_105;
const SYNTH_SIDE_EFFECT_EID: u16 = 65_001;
const SYNTH_PENALTY_ATTR_ID: u16 = 65_101;

fn shared_sde() -> Arc<SdeRegistry> {
    static SHARED_SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SHARED_SDE.get_or_init(|| {
        let mut sde = SdeRegistry::with_canonical_seed();

        // Inject synthetic booster side-effect attribute into Neurotoxin Control (25538) + synthetic booster effect
        let penalty_attr = sde
            .types
            .get(&25538)
            .and_then(|t| {
                t.effect_ids
                    .iter()
                    .filter_map(|eid| sde.effects.get(eid))
                    .flat_map(|eff| eff.modifiers.iter())
                    .find(|s| s.domain == "charID" && s.group_id == Some(303))
                    .map(|s| s.modified_attribute_id)
            })
            .unwrap_or(SYNTH_PENALTY_ATTR_ID);

        if let Some(mut base_eff) = sde.effects.values().next().cloned() {
            base_eff.effect_id = SYNTH_SIDE_EFFECT_EID;
            base_eff.effect_category = 0;
            base_eff.modifiers = vec![EffectModifierSpec {
                domain: "shipID".into(),
                func: "ItemModifier".into(),
                modified_attribute_id: eve_sde::attr_ids::CAPACITOR_CAPACITY,
                modifying_attribute_id: penalty_attr,
                operator: 6,
                group_id: None,
                skill_type_id: None,
                effect_id: None,
            }];
            sde.effects.insert(SYNTH_SIDE_EFFECT_EID, base_eff);
        }

        let proto = sde.types.values().next().unwrap().clone();
        let mut combat_b = proto.clone();
        combat_b.type_id = SYNTH_COMBAT_BOOSTER_TID;
        combat_b.category_id = 20;
        combat_b.group_id = 303;
        combat_b.volume = 1.0;
        combat_b.capacity = 0.0;
        combat_b.name = "Synthetic Combat Booster".into();
        combat_b.default_attributes.clear();
        combat_b.default_attributes.insert(1087, 1.0); // boosterness slot 1
        combat_b.default_attributes.insert(330, 1_800_000.0); // boosterDuration 1800s
        combat_b.default_attributes.insert(penalty_attr, -20.0);
        combat_b.effect_ids = vec![SYNTH_SIDE_EFFECT_EID];
        sde.types.insert(SYNTH_COMBAT_BOOSTER_TID, combat_b.clone());

        let mut accel_b = combat_b;
        accel_b.type_id = SYNTH_CEREBRAL_ACCEL_TID;
        accel_b.name = "Synthetic Cerebral Accelerator".into();
        accel_b.default_attributes.insert(1087, 2.0); // boosterness slot 2
        accel_b.default_attributes.insert(330, 3_600_000.0);
        accel_b.default_attributes.insert(1916, 1.0); // followsJumpClones
        accel_b.effect_ids.clear();
        sde.types.insert(SYNTH_CEREBRAL_ACCEL_TID, accel_b);

        if let Some(mut inj) = sde.types.get(&40520).cloned() {
            inj.type_id = SYNTH_NON_DIM_INJ_TID;
            inj.name = "Synthetic Non-Diminishing Skill Injector".into();
            inj.default_attributes
                .insert(ATTR_CONTAINED_SKILL_POINTS, 250_000.0);
            sde.types.insert(SYNTH_NON_DIM_INJ_TID, inj);
        }

        let mut imp_req = proto.clone();
        imp_req.type_id = SYNTH_REQ_IMPLANT_TID;
        imp_req.category_id = 20;
        imp_req.group_id = 300;
        imp_req.volume = 1.0;
        imp_req.name = "Synthetic Slot-1 Prereq Implant".into();
        imp_req.default_attributes.clear();
        imp_req.default_attributes.insert(331, 1.0); // implantness slot 1
        imp_req.default_attributes.insert(182, 3405.0); // requiredSkill1: Biology
        imp_req.default_attributes.insert(277, 3.0); // level 3
        sde.types.insert(SYNTH_REQ_IMPLANT_TID, imp_req.clone());

        let mut imp_plain = imp_req;
        imp_plain.type_id = SYNTH_SLOT1_IMPLANT_TID;
        imp_plain.name = "Synthetic Slot-1 Plain Implant".into();
        imp_plain.default_attributes.remove(&182);
        imp_plain.default_attributes.remove(&277);
        sde.types.insert(SYNTH_SLOT1_IMPLANT_TID, imp_plain);

        Arc::new(sde)
    }))
}

fn test_ctx() -> ServerContext {
    ServerContext::with_sde_and_repository(
        shared_sde(),
        Arc::new(eve_db::WorldRepository::with_demo_seed()),
    )
    .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED)
}

fn make_docked_session(char_id: i64, station_id: i64, sys_id: u32, ship_id: i64) -> SessionState {
    let mut s = SessionState::new(9721);
    s.user_id = Some(1);
    s.character_id = Some(char_id);
    s.corporation_id = Some(1000009);
    s.station_id = Some(station_id);
    s.solar_system_id = Some(sys_id);
    s.ship_id = Some(ship_id);
    s
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

fn dict_get<'a>(rep: &'a PyRep, key: &str) -> Option<&'a PyRep> {
    match rep {
        PyRep::KeyVal(map) => map.get(key),
        PyRep::Dict(entries) => entries.iter().find_map(|(k, v)| (k.as_str() == Some(key)).then_some(v)),
        PyRep::ObjectEx { header, .. } => {
            if let PyRep::Tuple(t) = header.as_ref() {
                if let Some(PyRep::Dict(entries)) = t.get(1) {
                    return entries.iter().find_map(|(k, v)| (k.as_str() == Some(key)).then_some(v));
                }
            }
            None
        }
        _ => None,
    }
}

#[test]
fn skill_injector_diminishing_returns_5m_50m_80m_brackets_and_daily_alpha_injector_rules() {
    // 1. Direct formula boundary checks at 5M, 50M, and 80M SP
    assert_eq!(skill_injector_sp_yield(0.0), 500_000.0);
    assert_eq!(skill_injector_sp_yield(4_999_999.0), 500_000.0);
    assert_eq!(skill_injector_sp_yield(5_000_000.0), 400_000.0);
    assert_eq!(skill_injector_sp_yield(49_999_999.0), 400_000.0);
    assert_eq!(skill_injector_sp_yield(50_000_000.0), 300_000.0);
    assert_eq!(skill_injector_sp_yield(79_999_999.0), 300_000.0);
    assert_eq!(skill_injector_sp_yield(80_000_000.0), 150_000.0);
    assert_eq!(skill_injector_sp_yield(150_000_000.0), 150_000.0);

    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let station_id = 60_003_760_i64;
    let sys_id = 30_000_142;
    let (pod_id, large_inj_id, small_inj_id, alpha_inj_id) = (71001, 71002, 71003, 71004);

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = Some(station_id);
            ch.solar_system_id = sys_id;
            ch.active_ship_id = pod_id;
            ch.clone_grade = 2; // Omega
            ch.free_skill_points = 0.0;
            ch.skill_points = 0.0;
        }
        snap.skills.entry(char_id).or_default().clear();
        snap.items.insert(pod_id, make_item(pod_id, 670, char_id, station_id, InventoryFlag::Hangar, 1, true));
        snap.items.insert(large_inj_id, make_item(large_inj_id, 40520, char_id, station_id, InventoryFlag::Hangar, 10, false));
        snap.items.insert(small_inj_id, make_item(small_inj_id, 45635, char_id, station_id, InventoryFlag::Hangar, 10, false));
        snap.items.insert(alpha_inj_id, make_item(alpha_inj_id, TYPE_DAILY_ALPHA_INJECTOR, char_id, station_id, InventoryFlag::Hangar, 5, false));
    });

    let mut session = make_docked_session(char_id, station_id, sys_id, pod_id);
    let kw = HashMap::new();

    // Test Large (40520) and Small (45635) Skill Injector across all 4 SP brackets
    for (trained_sp, expected_large, expected_small) in [
        (1_000_000.0_f64, 500_000.0_f64, 100_000.0_f64),
        (10_000_000.0, 400_000.0, 80_000.0),
        (60_000_000.0, 300_000.0, 60_000.0),
        (90_000_000.0, 150_000.0, 30_000.0),
    ] {
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&char_id) {
                ch.skill_points = trained_sp;
                ch.free_skill_points = 0.0;
            }
        });
        let l_rep = character_skills::handle(&ctx, &mut session, "skillMgr2", "InjectSkillpoints", &[PyRep::Integer(large_inj_id), PyRep::Integer(1)], &kw)
            .unwrap().unwrap();
        assert_eq!(l_rep.as_f64(), Some(expected_large));

        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&char_id) {
                ch.skill_points = trained_sp;
                ch.free_skill_points = 0.0;
            }
        });
        let s_rep = character_skills::handle(&ctx, &mut session, "skillMgr2", "InjectSkillpoints", &[PyRep::Integer(small_inj_id), PyRep::Integer(1)], &kw)
            .unwrap().unwrap();
        assert_eq!(s_rep.as_f64(), Some(expected_small));
    }

    // Daily Alpha Injector (47414) rejected for Omega clone (clone_grade == 2)
    let omega_err = character_skills::handle(&ctx, &mut session, "skillMgr2", "InjectSkillpoints", &[PyRep::Integer(alpha_inj_id), PyRep::Integer(1)], &kw)
        .unwrap().unwrap_err();
    assert_eq!(omega_err.code, "AlphaCloneRequired");

    // Switch to Alpha clone (clone_grade == 1) with < 5M SP: first Daily Alpha Injector succeeds (+50,000 SP), second fails on 24h cooldown
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.clone_grade = 1;
            ch.skill_points = 1_000_000.0;
            ch.free_skill_points = 0.0;
        }
    });
    let alpha_ok = character_skills::handle(&ctx, &mut session, "skillMgr2", "InjectSkillpoints", &[PyRep::Integer(alpha_inj_id), PyRep::Integer(1)], &kw)
        .unwrap().unwrap();
    assert_eq!(alpha_ok.as_f64(), Some(50_000.0));

    let cd_err = character_skills::handle(&ctx, &mut session, "skillMgr2", "InjectSkillpoints", &[PyRep::Integer(alpha_inj_id), PyRep::Integer(1)], &kw)
        .unwrap().unwrap_err();
    assert_eq!(cd_err.code, "DailyAlphaInjectorCooldownActive");
}

#[test]
fn booster_biology_duration_expiry_and_cerebral_accelerator_clone_jump_persistence() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let station_id = 60_003_760_i64;
    let sys_id = 30_000_142;
    let (pod_id, booster_item_1, booster_item_2) = (72000, 72001, 72002);

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = Some(station_id);
            ch.solar_system_id = sys_id;
            ch.active_ship_id = pod_id;
        }
        snap.skills.entry(char_id).or_default().remove(&3405);
        snap.items.retain(|_, i| !(i.owner_id == char_id && i.flag == InventoryFlag::Booster));
        snap.items.insert(pod_id, make_item(pod_id, 670, char_id, station_id, InventoryFlag::Hangar, 1, true));
        snap.items.insert(booster_item_1, make_item(booster_item_1, SYNTH_COMBAT_BOOSTER_TID, char_id, station_id, InventoryFlag::Hangar, 1, true));
        snap.items.insert(booster_item_2, make_item(booster_item_2, SYNTH_COMBAT_BOOSTER_TID, char_id, station_id, InventoryFlag::Hangar, 1, true));
    });

    let session = make_docked_session(char_id, station_id, sys_id, pod_id);

    // 1. Biology 0 vs Biology 5 (+100% duration) and BoosterSlotOccupied
    let rep_lv0 = use_booster(&ctx, &session, &[PyRep::Integer(booster_item_1)]).expect("booster lv0");
    let dur_lv0 = dict_get(&rep_lv0, "durationSeconds").and_then(|v| v.as_f64()).unwrap_or(0.0);
    assert!((dur_lv0 - 1_800.0).abs() < 1e-6);

    let slot_err = use_booster(&ctx, &session, &[PyRep::Integer(booster_item_2)]).unwrap_err();
    assert_eq!(slot_err.code, "BoosterSlotOccupied");

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.remove(&booster_item_1);
        let sk = snap.skills.entry(char_id).or_default();
        for tid in [3405u32, 25530, 25538] {
            sk.insert(tid, TrainedSkillRecord { type_id: tid, level: 5, skill_points: 256_000.0 });
        }
    });
    let rep_lv5 = use_booster(&ctx, &session, &[PyRep::Integer(booster_item_2)]).expect("booster lv5");
    let dur_lv5 = dict_get(&rep_lv5, "durationSeconds").and_then(|v| v.as_f64()).unwrap_or(0.0);
    assert!((dur_lv5 - dur_lv0 * 2.0).abs() < 1e-6);

    // 2. Side effect penalty adjustment scales ship attributes with Neurotoxin Control (25538): rolled vs unrolled
    let skill_levels = HashMap::from([(25538u32, 5u8)]);
    let mut rolled_attrs = HashMap::from([(eve_sde::attr_ids::CAPACITOR_CAPACITY, 800.0)]);
    let rolled_se = HashMap::from([(SYNTH_COMBAT_BOOSTER_TID, vec![u32::from(SYNTH_SIDE_EFFECT_EID)])]);
    apply_active_booster_side_effect_adjustments(&ctx.sde, &mut rolled_attrs, &skill_levels, &[SYNTH_COMBAT_BOOSTER_TID], &rolled_se);
    assert!((rolled_attrs[&eve_sde::attr_ids::CAPACITOR_CAPACITY] - 850.0).abs() < 1e-6);

    let mut unrolled_attrs = HashMap::from([(eve_sde::attr_ids::CAPACITOR_CAPACITY, 800.0)]);
    let unrolled_se = HashMap::from([(SYNTH_COMBAT_BOOSTER_TID, Vec::new())]);
    apply_active_booster_side_effect_adjustments(&ctx.sde, &mut unrolled_attrs, &skill_levels, &[SYNTH_COMBAT_BOOSTER_TID], &unrolled_se);
    assert!((unrolled_attrs[&eve_sde::attr_ids::CAPACITOR_CAPACITY] - 1000.0).abs() < 1e-6);

    // 3. Purge expired boosters and preserve followsJumpClones (attr 1916) boosters across clone jump
    let now_unix = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.retain(|_, i| !(i.owner_id == char_id && i.flag == InventoryFlag::Booster));
        let mut exp_b = make_item(72010, SYNTH_COMBAT_BOOSTER_TID, char_id, char_id, InventoryFlag::Booster, 1, true);
        exp_b.dynamic_attributes.insert(DYN_ATTR_BOOSTER_EXPIRY_UNIX, (now_unix.saturating_sub(10)) as f64);
        exp_b.dynamic_attributes.insert(DYN_ATTR_BOOSTER_DURATION_SECS, 1800.0);
        let mut combat_b = make_item(72011, SYNTH_COMBAT_BOOSTER_TID, char_id, char_id, InventoryFlag::Booster, 1, true);
        combat_b.dynamic_attributes.insert(DYN_ATTR_BOOSTER_EXPIRY_UNIX, (now_unix + 3600) as f64);
        let mut accel_b = make_item(72012, SYNTH_CEREBRAL_ACCEL_TID, char_id, char_id, InventoryFlag::Booster, 1, true);
        accel_b.dynamic_attributes.insert(DYN_ATTR_BOOSTER_EXPIRY_UNIX, (now_unix + 7200) as f64);

        snap.items.insert(72010, exp_b);
        snap.items.insert(72011, combat_b);
        snap.items.insert(72012, accel_b);
        snap.active_implants.insert(char_id, vec![9899]);

        let invalidated = purge_expired_boosters_in_snapshot(snap, Some(char_id), now_unix);
        assert_eq!(invalidated, vec![pod_id]);
        assert!(!snap.items.contains_key(&72010));

        sync_clone_jump_implants_and_boosters(&ctx.sde, snap, char_id);
        assert!(!snap.items.contains_key(&72011));
        assert!(snap.items.contains_key(&72012));
        assert!(snap.items.values().any(|i| i.owner_id == char_id && i.flag == InventoryFlag::Implant && i.type_id == 9899));
    });
}

#[test]
fn neural_remap_cooldown_and_bonus_remaps_and_jump_clone_install_jump_and_destroy_rules() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let station_a = 60_003_760_i64;
    let station_b = 60_008_494_i64;
    let sys_id = 30_000_142;
    let (pod_id, rifter_id) = (73001, 73002);

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = Some(station_a);
            ch.solar_system_id = sys_id;
            ch.active_ship_id = pod_id;
            ch.clone_grade = 1; // Start as Alpha to test OmegaCapacityRequired
            ch.balance = IskCents::from_isk(10_000_000);
            ch.neural.bonus_remaps = 1;
            ch.neural.last_remap_unix = None;
            ch.neural.remap_cooldown_until_unix = None;
            ch.last_clone_jump_unix = 0;
        }
        snap.jump_clones.insert(char_id, Vec::new());
        snap.items.insert(pod_id, make_item(pod_id, 670, char_id, station_a, InventoryFlag::Hangar, 1, true));
        snap.items.insert(rifter_id, make_item(rifter_id, 587, char_id, station_a, InventoryFlag::Hangar, 1, true));
    });

    let mut session = make_docked_session(char_id, station_a, sys_id, pod_id);
    let kw = HashMap::new();

    // 1. Neural Remap (`charMgr.RespecCharacter`):
    //    - Invalid total sum (!= 99) or out-of-range (< 17 or > 27) rejected with RemapFailed
    let bad_sum_err = character_skills::handle(
        &ctx, &mut session, "charMgr", "RespecCharacter",
        &[PyRep::Integer(20), PyRep::Integer(20), PyRep::Integer(20), PyRep::Integer(20), PyRep::Integer(20)], &kw,
    ).unwrap().unwrap_err();
    assert_eq!(bad_sum_err.code, "RemapFailed");

    //    - First valid remap (27 + 21 + 17 + 17 + 17 = 99) uses timed remap (bonus_remaps stays 1)
    let remap_1 = character_skills::handle(
        &ctx, &mut session, "charMgr", "RespecCharacter",
        &[PyRep::Integer(27), PyRep::Integer(21), PyRep::Integer(17), PyRep::Integer(17), PyRep::Integer(17)], &kw,
    ).unwrap().unwrap();
    assert_eq!(dict_get(&remap_1, "bonusRemaps").and_then(|v| v.as_i64()), Some(1));

    //    - Second remap during 365-day cooldown consumes bonus_remaps (1 -> 0)
    let remap_2 = character_skills::handle(
        &ctx, &mut session, "charMgr", "RespecCharacter",
        &[PyRep::Integer(17), PyRep::Integer(27), PyRep::Integer(21), PyRep::Integer(17), PyRep::Integer(17)], &kw,
    ).unwrap().unwrap();
    assert_eq!(dict_get(&remap_2, "bonusRemaps").and_then(|v| v.as_i64()), Some(0));

    //    - Third remap while timed cooldown is active and bonus_remaps == 0 fails with RemapFailed
    let remap_3_err = character_skills::handle(
        &ctx, &mut session, "charMgr", "RespecCharacter",
        &[PyRep::Integer(20), PyRep::Integer(20), PyRep::Integer(20), PyRep::Integer(20), PyRep::Integer(19)], &kw,
    ).unwrap().unwrap_err();
    assert_eq!(remap_3_err.code, "RemapFailed");

    // 2. Jump Clone Installation (`jumpCloneSvc.InstallCloneInStation`):
    //    - Alpha rejected with OmegaCapacityRequired
    let alpha_jc_err = character_skills::handle(&ctx, &mut session, "jumpCloneSvc", "InstallCloneInStation", &[], &kw)
        .unwrap().unwrap_err();
    assert_eq!(alpha_jc_err.code, "OmegaCapacityRequired");

    //    - Upgrade to Omega without Infomorph Psychology (24242) -> InfomorphPsychologyRequired
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().clone_grade = 2;
        snap.skills.entry(char_id).or_default().remove(&24242);
        snap.skills.entry(char_id).or_default().remove(&33407);
        snap.skills.entry(char_id).or_default().remove(&73910);
        snap.skills.entry(char_id).or_default().remove(&55025);
    });
    let no_skill_err = character_skills::handle(&ctx, &mut session, "jumpCloneSvc", "InstallCloneInStation", &[], &kw)
        .unwrap().unwrap_err();
    assert_eq!(no_skill_err.code, "InfomorphPsychologyRequired");

    //    - Grant Infomorph Psychology 1 (max 1 jump clone) -> installs clone at station_a, second install at station_a -> JumpCloneAlreadyAtStation
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(char_id).or_default().insert(
            24242,
            TrainedSkillRecord { type_id: 24242, level: 1, skill_points: 250.0 },
        );
    });
    let install_rep = character_skills::handle(&ctx, &mut session, "jumpCloneSvc", "InstallCloneInStation", &[], &kw)
        .unwrap().unwrap();
    let jc_a = dict_get(&install_rep, "jumpCloneID").and_then(|v| v.as_i64()).unwrap_or(0);
    assert!(jc_a > 0);

    let dup_err = character_skills::handle(&ctx, &mut session, "jumpCloneSvc", "InstallCloneInStation", &[], &kw)
        .unwrap().unwrap_err();
    assert_eq!(dup_err.code, "JumpCloneAlreadyAtStation");

    //    - Move to station_b: with Infomorph Psychology 1 and 1 clone already installed -> MaxJumpClonesReached
    session.station_id = Some(station_b);
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().station_id = Some(station_b);
    });
    let max_jc_err = character_skills::handle(&ctx, &mut session, "jumpCloneSvc", "InstallCloneInStation", &[], &kw)
        .unwrap().unwrap_err();
    assert_eq!(max_jc_err.code, "MaxJumpClonesReached");

    // 3. CloneJump rules:
    //    - Non-capsule active ship rejected with MustBeInCapsule
    session.ship_id = Some(rifter_id);
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().active_ship_id = rifter_id;
    });
    let non_pod_err = character_skills::handle(&ctx, &mut session, "jumpCloneSvc", "CloneJump", &[PyRep::Integer(jc_a)], &kw)
        .unwrap().unwrap_err();
    assert_eq!(non_pod_err.code, "MustBeInCapsule");

    //    - Switch back to capsule and remote CloneJump from station_b to jc_a (at station_a) -> succeeds and starts cooldown
    session.ship_id = Some(pod_id);
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().active_ship_id = pod_id;
    });
    let jump_rep = character_skills::handle(&ctx, &mut session, "jumpCloneSvc", "CloneJump", &[PyRep::Integer(jc_a)], &kw)
        .unwrap().unwrap();
    assert_eq!(dict_get(&jump_rep, "locationID").and_then(|v| v.as_i64()), Some(station_a));

    //    - DestroyInstalledClone removes the jump clone and returns Bool(true)
    let destroy_rep = character_skills::handle(&ctx, &mut session, "jumpCloneSvc", "DestroyInstalledClone", &[PyRep::Integer(jc_a)], &kw)
        .unwrap().unwrap();
    assert_eq!(destroy_rep, PyRep::Bool(true));
    assert!(ctx.db.with_snapshot(|s| {
        s.jump_clones.get(&char_id).map(|v| v.is_empty()).unwrap_or(true)
    }));
}

#[test]
fn skill_queue_catch_up_alpha_5m_cap_and_apply_free_skillpoints_rules() {
    let ctx = test_ctx();
    let (char_id, station_id, sys_id, pod_id) = (90_000_001_i64, 60_003_760_i64, 30_000_142_u32, 74001_i64);

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            (ch.station_id, ch.solar_system_id, ch.active_ship_id, ch.clone_grade) = (Some(station_id), sys_id, pod_id, 1);
            (ch.skill_points, ch.free_skill_points) = (5_000_000.0, 50_000.0);
        }
        snap.items.insert(pod_id, make_item(pod_id, 670, char_id, station_id, InventoryFlag::Hangar, 1, true));
        let sk = snap.skills.entry(char_id).or_default();
        sk.remove(&3405);
        sk.remove(&3413);
        sk.insert(3402, TrainedSkillRecord { type_id: 3402, level: 0, skill_points: 0.0 });
        sk.insert(3412, TrainedSkillRecord { type_id: 3412, level: 0, skill_points: 0.0 });
    });

    let (mut session, kw) = (make_docked_session(char_id, station_id, sys_id, pod_id), HashMap::new());
    let skill_args = &[PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(3402), PyRep::Integer(1)])])];
    let q_cap_err = character_skills::handle(&ctx, &mut session, "skillMgr2", "SaveSkillQueue", skill_args, &kw).unwrap().unwrap_err();
    assert_eq!(q_cap_err.code, "AlphaCloneSpCapExceeded");
    let free_cap_err = character_skills::handle(&ctx, &mut session, "skillMgr2", "ApplyFreeSkillPoints", &[PyRep::Integer(3402), PyRep::Real(250.0)], &kw).unwrap().unwrap_err();
    assert_eq!(free_cap_err.code, "AlphaCloneSpCapExceeded");

    ctx.db.with_mut_snapshot(|snap| { snap.characters.get_mut(&char_id).unwrap().skill_points = 100_000.0; });
    let prereq_args = &[PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(3412), PyRep::Integer(1)])])];
    assert_eq!(character_skills::handle(&ctx, &mut session, "skillMgr2", "SaveSkillQueue", prereq_args, &kw).unwrap().unwrap_err().code, "SkillPrerequisiteNotMet");
    assert_eq!(character_skills::handle(&ctx, &mut session, "skillMgr2", "ApplyFreeSkillPoints", &[PyRep::Integer(25538), PyRep::Real(500.0)], &kw).unwrap().unwrap_err().code, "SkillNotInjected");

    let lv1_sp = sp_for_skill_level(1.0, 1);
    character_skills::handle(&ctx, &mut session, "skillMgr2", "ApplyFreeSkillPoints", &[PyRep::Integer(3402), PyRep::Real(lv1_sp)], &kw).unwrap().unwrap();
    assert_eq!(ctx.db.with_snapshot(|s| s.skills[&char_id][&3402].level), 1);

    ctx.db.with_mut_snapshot(|snap| { snap.characters.get_mut(&char_id).unwrap().clone_grade = 2; });
    let lv2_args = &[PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(3402), PyRep::Integer(2)])])];
    character_skills::handle(&ctx, &mut session, "skillMgr2", "SaveSkillQueue", lv2_args, &kw).unwrap().unwrap();
    let now_u = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(q) = snap.skill_queues.get_mut(&char_id).and_then(|v| v.first_mut()) {
            (q.duration_seconds, q.started_at_unix) = (100.0, Some(now_u.saturating_sub(10_000)));
        }
    });
    catch_up_skill_queue(&ctx, char_id);
    assert_eq!(ctx.db.with_snapshot(|s| s.skills[&char_id][&3402].level), 2);
    character_skills::handle(&ctx, &mut session, "skillMgr2", "SaveSkillQueue", &[PyRep::List(vec![])], &kw).unwrap().unwrap();
    let info_rep = character_skills::handle(&ctx, &mut session, "charMgr", "GetCharacterInfo", &[], &kw).unwrap().unwrap();
    assert_eq!(dict_get(&info_rep, "characterID").and_then(|v| v.as_i64()), Some(char_id));
}

#[test]
fn implant_slot_stacking_prerequisites_and_non_diminishing_injector_rules() {
    let ctx = test_ctx();
    let (char_id, station_id, sys_id) = (90_000_001_i64, 60_003_760_i64, 30_000_142_u32);
    let (pod_id, imp_stack_id, imp_dup_id, nd_inj_id, exp_booster_id) = (75001, 75002, 75003, 75004, 75005);

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = Some(station_id);
            ch.solar_system_id = sys_id;
            ch.active_ship_id = pod_id;
            ch.skill_points = 90_000_000.0;
            ch.free_skill_points = 0.0;
        }
        snap.items.retain(|_, i| !(i.owner_id == char_id && matches!(i.flag, InventoryFlag::Implant | InventoryFlag::Booster)));
        snap.skills.entry(char_id).or_default().remove(&3405);
        snap.items.insert(pod_id, make_item(pod_id, 670, char_id, station_id, InventoryFlag::Hangar, 1, true));
        snap.items.insert(imp_stack_id, make_item(imp_stack_id, SYNTH_REQ_IMPLANT_TID, char_id, station_id, InventoryFlag::Hangar, 3, false));
        snap.items.insert(imp_dup_id, make_item(imp_dup_id, SYNTH_SLOT1_IMPLANT_TID, char_id, station_id, InventoryFlag::Hangar, 1, true));
        snap.items.insert(nd_inj_id, make_item(nd_inj_id, SYNTH_NON_DIM_INJ_TID, char_id, station_id, InventoryFlag::Hangar, 2, false));
        let mut exp_b = make_item(exp_booster_id, SYNTH_COMBAT_BOOSTER_TID, char_id, char_id, InventoryFlag::Booster, 1, true);
        exp_b.dynamic_attributes.insert(DYN_ATTR_BOOSTER_EXPIRY_UNIX, (ctx.now_unix().saturating_sub(5)) as f64);
        exp_b.dynamic_attributes.insert(DYN_ATTR_BOOSTER_DURATION_SECS, 1800.0);
        snap.items.insert(exp_booster_id, exp_b);
    });

    let mut session = make_docked_session(char_id, station_id, sys_id, pod_id);
    let kw = HashMap::new();

    // 1. Implant prerequisite check -> grant Biology 3 -> inject from stack of 3 (leaves qty 2)
    let prereq_err = character_skills::handle(&ctx, &mut session, "dogmaIM", "InjectImplant", &[PyRep::Integer(imp_stack_id)], &kw).unwrap().unwrap_err();
    assert_eq!(prereq_err.code, "SkillPrerequisitesNotMet");

    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(char_id).or_default().insert(3405, TrainedSkillRecord { type_id: 3405, level: 3, skill_points: 45_255.0 });
    });
    character_skills::handle(&ctx, &mut session, "dogmaIM", "InjectImplant", &[PyRep::Integer(imp_stack_id)], &kw).unwrap().unwrap();
    assert_eq!(ctx.db.with_snapshot(|s| s.items.get(&imp_stack_id).map(|i| i.quantity)), Some(2));

    // 2. Second slot-1 implant fails with ImplantSlotOccupied; DestroyImplant clears slot
    let slot_err = character_skills::handle(&ctx, &mut session, "dogmaIM", "InjectImplant", &[PyRep::Integer(imp_dup_id)], &kw).unwrap().unwrap_err();
    assert_eq!(slot_err.code, "ImplantSlotOccupied");

    let impl_list = character_skills::handle(&ctx, &mut session, "skillMgr2", "GetImplants", &[], &kw).unwrap().unwrap();
    let PyRep::Dict(impls) = impl_list else { panic!("expected implants dict") };
    assert_eq!(impls.len(), 1);
    let plugged_id = dict_get(&impls[0].1, "itemID").and_then(|v| v.as_i64()).unwrap();
    character_skills::handle(&ctx, &mut session, "dogmaIM", "DestroyImplant", &[PyRep::Integer(plugged_id)], &kw).unwrap().unwrap();
    assert!(ctx.db.with_snapshot(|s| !s.items.contains_key(&plugged_id)));

    // 3. GetBoosters purges expired booster; non-diminishing injector yields full 250,000 SP at 90M SP
    let boosters_rep = character_skills::handle(&ctx, &mut session, "skillMgr2", "GetBoosters", &[], &kw).unwrap().unwrap();
    assert_eq!(boosters_rep, PyRep::Dict(vec![]));

    let nd_sp = character_skills::handle(&ctx, &mut session, "skillMgr2", "InjectSkillpoints", &[PyRep::Integer(nd_inj_id), PyRep::Integer(1)], &kw).unwrap().unwrap();
    assert_eq!(nd_sp.as_f64(), Some(250_000.0));
    assert!(handle_skill_notifications_and_injections(&ctx, &session, "nonDiminishingInjectionMgr", "GetAvailableNonDiminishingInjections").is_some());
    assert!(handle_skill_notifications_and_injections(&ctx, &session, "alphaInjectorMgr", "GetNextAvailableInjection").is_some());
}
