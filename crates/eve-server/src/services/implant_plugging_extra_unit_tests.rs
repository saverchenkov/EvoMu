//! Deep branch-coverage and mutation-killing unit tests for `services::implant_plugging`.

#![forbid(unsafe_code)]

use super::implant_plugging::*;
use crate::router::ServerContext;
use eve_db::{ServerClock, clock::VirtualServerClock};
use eve_db::{
    InventoryFlag, InventoryItemRecord, OwnerNoteRecord, TrainedSkillRecord, WorldRepository,
};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_sde::{DogmaEffectType, EffectModifierSpec, InvType, SdeRegistry};
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

fn make_ctx_with_sde<F: FnOnce(&mut SdeRegistry)>(
    base_unix: u64,
    mutate_sde: F,
) -> (ServerContext, Arc<VirtualServerClock>) {
    let mut sde = (*shared_sde()).clone();
    mutate_sde(&mut sde);
    let db = Arc::new(WorldRepository::with_demo_seed());
    let ctx = ServerContext::with_sde_and_repository(Arc::new(sde), db);
    let clock = Arc::new(VirtualServerClock::with_fixed_base(base_unix));
    ctx.set_clock(clock.clone());
    (ctx, clock)
}

fn docked_sess(char_id: i64, station_id: i64) -> SessionState {
    let mut s = SessionState::new(701);
    s.user_id = Some(1);
    s.character_id = Some(char_id);
    s.corporation_id = Some(98_000_001);
    s.solar_system_id = Some(30_000_142);
    s.station_id = Some(station_id);
    s
}

fn call_skill_inj_rpc(
    ctx: &ServerContext,
    sess: &SessionState,
    svc: &str,
    method: &str,
) -> Option<Result<PyRep, BridgeRpcError>> {
    handle_skill_notifications_and_injections(ctx, sess, svc, method)
}

#[test]
fn inject_and_destroy_implant_all_validation_stack_split_and_type_id_removal_branches() {
    let custom_imp_tid = 999_501_u32;
    let custom_non_imp_tid = 999_502_u32;
    let req_skill_tid = 3411_u32; // Cybernetics

    let (ctx, _) = make_ctx_with_sde(1_775_000_000, |sde| {
        let mut imp: InvType = sde.types.get(&587).unwrap().clone();
        imp.type_id = custom_imp_tid;
        imp.name = "Test Slot 3 Implant".into();
        imp.default_attributes.clear();
        imp.default_attributes.insert(ATTR_IMPLANTNESS, 3.0);
        imp.default_attributes.insert(182, f64::from(req_skill_tid)); // requiredSkill1
        imp.default_attributes.insert(277, 4.0); // requiredSkill1Level = 4
        sde.types.insert(custom_imp_tid, imp);

        let mut non_imp: InvType = sde.types.get(&587).unwrap().clone();
        non_imp.type_id = custom_non_imp_tid;
        non_imp.default_attributes.clear();
        non_imp.default_attributes.insert(ATTR_IMPLANTNESS, 11.0); // out of 1..=10 range
        sde.types.insert(custom_non_imp_tid, non_imp);
    });

    let char_id = 90_000_001_i64;
    let st_id = 60_003_760_i64;
    let sess = docked_sess(char_id, st_id);

    ctx.db.with_mut_snapshot(|snap| {
        snap.active_implants.remove(&char_id);
        snap.skills.remove(&char_id);
        // Foreign-owned implant
        snap.items.insert(
            810_001,
            InventoryItemRecord {
                item_id: 810_001,
                type_id: custom_imp_tid,
                owner_id: 90_000_002,
                location_id: st_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        // Already plugged-in implant
        snap.items.insert(
            810_002,
            InventoryItemRecord {
                item_id: 810_002,
                type_id: custom_imp_tid,
                owner_id: char_id,
                location_id: char_id,
                flag: InventoryFlag::Implant,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        // Unreachable implant (at another station)
        snap.items.insert(
            810_003,
            InventoryItemRecord {
                item_id: 810_003,
                type_id: custom_imp_tid,
                owner_id: char_id,
                location_id: st_id + 99,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        // Non-implant item (slot 11)
        snap.items.insert(
            810_004,
            InventoryItemRecord {
                item_id: 810_004,
                type_id: custom_non_imp_tid,
                owner_id: char_id,
                location_id: st_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        // Stack of 3 valid implants in Hangar
        snap.items.insert(
            810_005,
            InventoryItemRecord {
                item_id: 810_005,
                type_id: custom_imp_tid,
                owner_id: char_id,
                location_id: st_id,
                flag: InventoryFlag::Hangar,
                quantity: 3,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });

    assert_eq!(
        inject_implant(&ctx, &sess, &[PyRep::Integer(810_001)])
            .unwrap_err()
            .code,
        "ItemNotOwned"
    );
    assert_eq!(
        inject_implant(&ctx, &sess, &[PyRep::Integer(810_002)])
            .unwrap_err()
            .code,
        "ImplantAlreadyPluggedIn"
    );
    assert_eq!(
        inject_implant(&ctx, &sess, &[PyRep::Integer(810_003)])
            .unwrap_err()
            .code,
        "ItemNotReachable"
    );
    assert_eq!(
        inject_implant(&ctx, &sess, &[PyRep::Integer(810_004)])
            .unwrap_err()
            .code,
        "ItemNotAnImplant"
    );
    // Missing Cybernetics IV skill prerequisite
    assert!(inject_implant(&ctx, &sess, &[PyRep::Integer(810_005)]).is_err());

    // Grant Cybernetics IV, but 810_002 is still occupying slot 3 -> ImplantSlotOccupied
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(char_id).or_default().insert(
            req_skill_tid,
            TrainedSkillRecord {
                type_id: req_skill_tid,
                level: 4,
                skill_points: 50_000.0,
            },
        );
    });
    assert_eq!(
        inject_implant(&ctx, &sess, &[PyRep::Integer(810_005)])
            .unwrap_err()
            .code,
        "ImplantSlotOccupied"
    );

    // Destroy 810_002 by passing its type_id (custom_imp_tid) instead of item_id!
    assert_eq!(
        destroy_implant(&ctx, &sess, &[PyRep::Integer(810_004)])
            .unwrap_err()
            .code,
        "ImplantNotPluggedIn"
    );
    assert_eq!(
        destroy_implant(&ctx, &sess, &[PyRep::Integer(-5)])
            .unwrap_err()
            .code,
        "ImplantNotFound"
    );
    assert_eq!(
        destroy_implant(&ctx, &sess, &[PyRep::Integer(i64::from(custom_imp_tid))]).unwrap(),
        PyRep::None
    );

    // Now inject from stack 810_005 (qty 3 -> 2, and new singleton Implant item minted)
    assert_eq!(
        inject_implant(&ctx, &sess, &[PyRep::Integer(810_005)]).unwrap(),
        PyRep::None
    );
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items[&810_005].quantity, 2);
        assert_eq!(snap.active_implants[&char_id], vec![custom_imp_tid]);
    });
}

#[test]
fn booster_biology_duration_neurotoxin_side_effects_expiry_and_clone_jump_sync() {
    let booster_tid = 999_601_u32;
    let persistent_booster_tid = 999_602_u32;
    let penalty_attr_id = 1095_u16;
    let ship_attr_id = 37_u16; // maxVelocity
    let side_eff_1: u16 = 9901;
    let side_eff_2: u16 = 9902;

    let (ctx, _) = make_ctx_with_sde(1_775_000_000, |sde| {
        // Ensure Neurotoxin Control (25538) references penalty_attr_id on charID group 303
        let nc_eff_id = 9900_u16;
        sde.effects.insert(
            nc_eff_id,
            DogmaEffectType {
                effect_id: nc_eff_id,
                name: "neurotoxinControlTest".into(),
                modifiers: vec![EffectModifierSpec {
                    func: "LocationGroupModifier".into(),
                    domain: "charID".into(),
                    group_id: Some(303),
                    skill_type_id: None,
                    modified_attribute_id: penalty_attr_id,
                    modifying_attribute_id: 1126,
                    operator: 6,
                    effect_id: None,
                }],
                ..Default::default()
            },
        );
        if let Some(nc_type) = sde.types.get_mut(&SKILL_NEUROTOXIN_CONTROL) {
            nc_type.effect_ids.push(nc_eff_id);
            nc_type.default_attributes.insert(1126, -5.0);
        }

        // Side effect 1 & 2 on shipID
        for eid in [side_eff_1, side_eff_2] {
            sde.effects.insert(
                eid,
                DogmaEffectType {
                    effect_id: eid,
                    name: format!("boosterSideEff{eid}"),
                    modifiers: vec![EffectModifierSpec {
                        func: "ItemModifier".into(),
                        domain: "shipID".into(),
                        group_id: None,
                        skill_type_id: None,
                        modified_attribute_id: ship_attr_id,
                        modifying_attribute_id: penalty_attr_id,
                        operator: 6,
                        effect_id: None,
                    }],
                    ..Default::default()
                },
            );
        }

        let mut b1: InvType = sde.types.get(&587).unwrap().clone();
        b1.type_id = booster_tid;
        b1.group_id = 303;
        b1.default_attributes.clear();
        b1.default_attributes.insert(ATTR_BOOSTERNESS, 1.0);
        b1.default_attributes
            .insert(ATTR_BOOSTER_DURATION_MS, 1_800_000.0); // 1800s (30m)
        // Slot 0 threshold is 0.10, Slot 1 threshold is 0.18.
        // Set chance1 = 0.25 (triggers), chance2 = 0.12 (does not trigger at 0.18)
        b1.default_attributes
            .insert(ATTR_BOOSTER_EFFECT_CHANCE_1, 0.25);
        b1.default_attributes
            .insert(ATTR_BOOSTER_EFFECT_CHANCE_1 + 1, 0.12);
        b1.default_attributes.insert(penalty_attr_id, -20.0); // -20% velocity penalty
        b1.effect_ids = vec![side_eff_1, side_eff_2];
        sde.types.insert(booster_tid, b1.clone());

        let mut b2 = b1;
        b2.type_id = persistent_booster_tid;
        b2.default_attributes.insert(ATTR_BOOSTERNESS, 2.0);
        b2.default_attributes.insert(ATTR_FOLLOWS_JUMP_CLONES, 1.0); // Cerebral Accelerator
        sde.types.insert(persistent_booster_tid, b2);
    });

    let char_id = 90_000_001_i64;
    let st_id = 60_003_760_i64;
    let sess = docked_sess(char_id, st_id);

    ctx.db.with_mut_snapshot(|snap| {
        let sk = snap.skills.entry(char_id).or_default();
        sk.insert(
            SKILL_BIOLOGY,
            TrainedSkillRecord {
                type_id: SKILL_BIOLOGY,
                level: 5, // +100% duration -> 3600s
                skill_points: 256_000.0,
            },
        );
        sk.insert(
            SKILL_NEUROTOXIN_RECOVERY,
            TrainedSkillRecord {
                type_id: SKILL_NEUROTOXIN_RECOVERY,
                level: 4, // -20% side-effect chance -> 0.25 * 0.8 = 0.20 > 0.10
                skill_points: 50_000.0,
            },
        );
        // Stack of 2 combat boosters + 1 cerebral accelerator
        snap.items.insert(
            820_001,
            InventoryItemRecord {
                item_id: 820_001,
                type_id: booster_tid,
                owner_id: char_id,
                location_id: st_id,
                flag: InventoryFlag::Hangar,
                quantity: 2,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            820_002,
            InventoryItemRecord {
                item_id: 820_002,
                type_id: persistent_booster_tid,
                owner_id: char_id,
                location_id: st_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });

    // Wrong expected_loc (2nd argument) -> ItemNotReachable
    assert_eq!(
        use_booster(
            &ctx,
            &sess,
            &[PyRep::Integer(820_001), PyRep::Integer(st_id + 1)]
        )
        .unwrap_err()
        .code,
        "ItemNotReachable"
    );

    // Consume 1 unit from stack 820_001 at correct location
    let rep1 = use_booster(
        &ctx,
        &sess,
        &[PyRep::Integer(820_001), PyRep::Integer(st_id)],
    )
    .unwrap();
    assert_eq!(
        rep1.dict_get("durationSeconds"),
        Some(&PyRep::Integer(3_600))
    );
    assert_eq!(
        rep1.dict_get("sideEffectIDs"),
        Some(&PyRep::List(vec![PyRep::Integer(i64::from(side_eff_1))]))
    );

    // Consuming second unit while slot 1 is still active -> BoosterSlotOccupied
    assert_eq!(
        use_booster(&ctx, &sess, &[PyRep::Integer(820_001)])
            .unwrap_err()
            .code,
        "BoosterSlotOccupied"
    );

    // Consume persistent booster in slot 2
    use_booster(&ctx, &sess, &[PyRep::Integer(820_002)]).unwrap();

    // Test apply_active_booster_side_effect_adjustments:
    // side_eff_1 IS rolled (with Neurotoxin Control V -> -25% penalty severity: -20% * 0.75 = -15%, factor 0.85 / 0.80)
    // side_eff_2 is NOT rolled (restored: factor 1.0 / 0.80)
    let (act_bsts, rolled_map) = active_combat_boosters_for_char(&ctx.db, char_id, 1_775_000_000);
    assert_eq!(act_bsts.len(), 2);
    let mut ship_attrs = HashMap::from([(ship_attr_id, 64.0)]); // 100.0 * 0.8 * 0.8 = 64.0
    let mut skill_lvls = HashMap::new();
    skill_lvls.insert(SKILL_NEUROTOXIN_CONTROL, 5);
    apply_active_booster_side_effect_adjustments(
        &ctx.sde,
        &mut ship_attrs,
        &skill_lvls,
        &[booster_tid],
        &rolled_map,
    );
    // 64.0 * (0.85 / 0.80) * (1.0 / 0.80) = 85.0
    assert!((ship_attrs[&ship_attr_id] - 85.0).abs() < 1e-6);

    // Clone jump sync removes slot 1 booster (followsJumpClones < 1.0) but keeps slot 2 cerebral accelerator!
    ctx.db.with_mut_snapshot(|snap| {
        sync_clone_jump_implants_and_boosters(&ctx.sde, snap, char_id);
    });
    let (after_jump, _) = active_combat_boosters_for_char(&ctx.db, char_id, 1_775_000_000);
    assert_eq!(after_jump, vec![persistent_booster_tid]);

    // Expiry purge at exact boundary (1_775_000_000 + 3_600)
    purge_expired_boosters_all(&ctx, 1_775_003_599);
    assert_eq!(
        active_combat_boosters_for_char(&ctx.db, char_id, 1_775_003_599)
            .0
            .len(),
        1
    );
    purge_expired_boosters_all(&ctx, 1_775_003_600);
    assert!(
        active_combat_boosters_for_char(&ctx.db, char_id, 1_775_003_600)
            .0
            .is_empty()
    );
}

#[test]
fn daily_alpha_small_and_non_diminishing_skill_injectors_and_cooldown_rpcs() {
    let custom_fixed_inj_tid = 999_701_u32;
    let (ctx, _) = make_ctx_with_sde(1_775_000_000, |sde| {
        let mut fixed_inj: InvType = sde.types.get(&587).unwrap().clone();
        fixed_inj.type_id = custom_fixed_inj_tid;
        fixed_inj.default_attributes.clear();
        fixed_inj
            .default_attributes
            .insert(ATTR_CONTAINED_SKILL_POINTS, 150_000.0);
        sde.types.insert(custom_fixed_inj_tid, fixed_inj);
    });

    assert_eq!(
        non_diminishing_sp_yield(&ctx.sde, custom_fixed_inj_tid),
        Some(150_000.0)
    );
    let char_id = 90_000_001_i64;
    let st_id = 60_003_760_i64;
    let sess = docked_sess(char_id, st_id);

    // 1. No injectors in hangar -> InjectorNotFound
    assert_eq!(
        inject_skillpoints(&ctx, &sess, &[PyRep::Integer(999_999)])
            .unwrap_err()
            .code,
        "InjectorNotFound"
    );

    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().clone_grade = 2; // Omega
        snap.items.insert(
            830_001,
            InventoryItemRecord {
                item_id: 830_001,
                type_id: TYPE_DAILY_ALPHA_INJECTOR,
                owner_id: char_id,
                location_id: st_id,
                flag: InventoryFlag::Hangar,
                quantity: 2,
                singleton: false,
                ..Default::default()
            },
        );
        snap.items.insert(
            830_002,
            InventoryItemRecord {
                item_id: 830_002,
                type_id: TYPE_SMALL_SKILL_INJECTOR,
                owner_id: char_id,
                location_id: st_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: false,
                ..Default::default()
            },
        );
        snap.items.insert(
            830_003,
            InventoryItemRecord {
                item_id: 830_003,
                type_id: custom_fixed_inj_tid,
                owner_id: char_id,
                location_id: st_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: false,
                ..Default::default()
            },
        );
    });

    // 2. Omega character rejected on Daily Alpha Injector -> AlphaCloneRequired
    assert_eq!(
        inject_skillpoints(&ctx, &sess, &[PyRep::Integer(830_001)])
            .unwrap_err()
            .code,
        "AlphaCloneRequired"
    );

    // 3. Switch to Alpha (clone_grade = 1): first Daily Alpha Injector succeeds (50,000 SP),
    //    second within 24h fails with DailyAlphaInjectorCooldownActive
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().clone_grade = 1;
    });
    assert_eq!(
        inject_skillpoints(&ctx, &sess, &[PyRep::Integer(830_001)])
            .unwrap()
            .as_f64(),
        Some(50_000.0)
    );
    assert_eq!(
        inject_skillpoints(&ctx, &sess, &[PyRep::Integer(830_001)])
            .unwrap_err()
            .code,
        "DailyAlphaInjectorCooldownActive"
    );

    // GetNextAvailableInjection returns > 0 while cooldown is active
    let next_ft = call_skill_inj_rpc(
        &ctx,
        &sess,
        "alphaInjectorMgr",
        "GetNextAvailableInjection",
    )
    .unwrap()
    .unwrap()
    .as_i64()
    .unwrap();
    assert!(next_ft > ctx.now_blueos_filetime_ticks());

    // 4. Small Skill Injector (830_002) and custom non-diminishing injector (830_003)
    let small_sp = inject_skillpoints(&ctx, &sess, &[PyRep::Integer(830_002)])
        .unwrap()
        .as_f64()
        .unwrap();
    assert!((30_000.0..=100_000.0).contains(&small_sp));

    // Remove remaining Daily Alpha Injector so 830_003 is the last non-diminishing injector (triggers OnNonDiminishingInjectionsRemoved)
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.remove(&830_001);
        // Also test fallback loading of cooldowns when owner_notes has invalid JSON
        snap.owner_notes.insert(
            ALPHA_INJECTOR_COOLDOWNS_KEY,
            OwnerNoteRecord {
                note_id: ALPHA_INJECTOR_COOLDOWNS_KEY,
                character_id: -10,
                item_id: 1,
                label: "bad".into(),
                note: "not_json".into(),
                timestamp_unix: 0,
            },
        );
    });
    assert!(!ctx
        .db
        .with_snapshot(|snap| load_alpha_injector_cooldowns(snap))
        .is_empty());

    assert_eq!(
        inject_skillpoints(&ctx, &sess, &[PyRep::Integer(830_003)])
            .unwrap()
            .as_f64(),
        Some(150_000.0)
    );
}

#[test]
fn implant_plugging_additional_boundaries_space_cargo_and_mutant_killing_math() {
    const SYN_ZERO_YIELD_INJ: u32 = 998_101;
    const SYN_ZERO_DUR_BOOSTER: u32 = 998_102;
    const SYN_BOUNDARY_BOOSTER: u32 = 998_103;
    const SYN_NEG100_PEN_BOOSTER: u32 = 998_104;
    const SYN_IMPLANT_SLOT_4: u32 = 998_105;
    const SYN_IMPLANT_SLOT_6: u32 = 998_106;

    const PEN_ATTR: u16 = 998;
    const MOD_SHIP_ATTR: u16 = 999;
    const EFF_THRESH_10: u16 = 59_811;
    const EFF_NEG100: u16 = 59_812;
    const EFF_NON_SHIP_DOM: u16 = 59_813;

    let (base_ctx, clock) = make_ctx_with_sde(1_780_000_000, |sde| {
        if let Some(nc) = sde.types.get_mut(&SKILL_NEUROTOXIN_CONTROL) {
            nc.effect_ids.push(59_810);
        }
        sde.effects.insert(
            59_810,
            DogmaEffectType {
                effect_id: 59_810,
                name: "ncPenaltyRef".into(),
                effect_category: 0,
                modifiers: vec![EffectModifierSpec {
                    domain: "charID".into(),
                    func: "ItemModifier".into(),
                    modified_attribute_id: PEN_ATTR,
                    modifying_attribute_id: 1126,
                    operator: 6,
                    group_id: Some(303),
                    skill_type_id: None,
                    effect_id: None,
                }],
                ..Default::default()
            },
        );
        sde.effects.insert(
            EFF_THRESH_10,
            DogmaEffectType {
                effect_id: EFF_THRESH_10,
                name: "effThresh10".into(),
                effect_category: 0,
                modifiers: vec![EffectModifierSpec {
                    domain: "shipID".into(),
                    func: "ItemModifier".into(),
                    modified_attribute_id: MOD_SHIP_ATTR,
                    modifying_attribute_id: PEN_ATTR,
                    operator: 6,
                    group_id: None,
                    skill_type_id: None,
                    effect_id: None,
                }],
                ..Default::default()
            },
        );
        sde.effects.insert(
            EFF_NEG100,
            DogmaEffectType {
                effect_id: EFF_NEG100,
                name: "effNeg100".into(),
                effect_category: 0,
                modifiers: vec![EffectModifierSpec {
                    domain: "shipID".into(),
                    func: "ItemModifier".into(),
                    modified_attribute_id: MOD_SHIP_ATTR,
                    modifying_attribute_id: PEN_ATTR,
                    operator: 6,
                    group_id: None,
                    skill_type_id: None,
                    effect_id: None,
                }],
                ..Default::default()
            },
        );
        sde.effects.insert(
            EFF_NON_SHIP_DOM,
            DogmaEffectType {
                effect_id: EFF_NON_SHIP_DOM,
                name: "effCharDomain".into(),
                effect_category: 0,
                modifiers: vec![EffectModifierSpec {
                    domain: "charID".into(),
                    func: "ItemModifier".into(),
                    modified_attribute_id: MOD_SHIP_ATTR,
                    modifying_attribute_id: PEN_ATTR,
                    operator: 6,
                    group_id: None,
                    skill_type_id: None,
                    effect_id: None,
                }],
                ..Default::default()
            },
        );

        let template = sde.types.get(&587).unwrap().clone();
        let mut inj_zero = template.clone();
        inj_zero.type_id = SYN_ZERO_YIELD_INJ;
        inj_zero.group_id = 1739;
        inj_zero.category_id = 9;
        inj_zero.name = "Zero Yield Injector".into();
        inj_zero.default_attributes.clear();
        inj_zero
            .default_attributes
            .insert(ATTR_CONTAINED_SKILL_POINTS, 0.0);
        sde.types.insert(SYN_ZERO_YIELD_INJ, inj_zero);

        let mut zero_dur = template.clone();
        zero_dur.type_id = SYN_ZERO_DUR_BOOSTER;
        zero_dur.group_id = 303;
        zero_dur.category_id = 20;
        zero_dur.name = "Zero Dur Booster".into();
        zero_dur.default_attributes.clear();
        zero_dur.default_attributes.insert(ATTR_BOOSTERNESS, 2.0);
        zero_dur
            .default_attributes
            .insert(ATTR_BOOSTER_DURATION_MS, 0.0);
        sde.types.insert(SYN_ZERO_DUR_BOOSTER, zero_dur);

        let mut boundary_b = template.clone();
        boundary_b.type_id = SYN_BOUNDARY_BOOSTER;
        boundary_b.group_id = 303;
        boundary_b.category_id = 20;
        boundary_b.name = "Boundary Booster".into();
        boundary_b.default_attributes.clear();
        boundary_b.default_attributes.insert(ATTR_BOOSTERNESS, 3.0);
        boundary_b
            .default_attributes
            .insert(ATTR_BOOSTER_DURATION_MS, 1_000_000.0);
        // Exact boundary 0.10: roll_threshold for idx 0 is 0.10, `0.10 < 0.10` is false -> does NOT roll!
        boundary_b
            .default_attributes
            .insert(ATTR_BOOSTER_EFFECT_CHANCE_1, 0.10);
        boundary_b.default_attributes.insert(PEN_ATTR, -20.0);
        boundary_b.effect_ids = vec![EFF_THRESH_10, EFF_NON_SHIP_DOM];
        sde.types.insert(SYN_BOUNDARY_BOOSTER, boundary_b);

        let mut neg100_b = template.clone();
        neg100_b.type_id = SYN_NEG100_PEN_BOOSTER;
        neg100_b.group_id = 303;
        neg100_b.category_id = 20;
        neg100_b.name = "Neg100 Pen Booster".into();
        neg100_b.default_attributes.clear();
        neg100_b.default_attributes.insert(ATTR_BOOSTERNESS, 4.0);
        neg100_b
            .default_attributes
            .insert(ATTR_BOOSTER_DURATION_MS, 500_000.0);
        neg100_b.default_attributes.insert(PEN_ATTR, -100.0);
        neg100_b.effect_ids = vec![EFF_NEG100, EFF_NON_SHIP_DOM];
        sde.types.insert(SYN_NEG100_PEN_BOOSTER, neg100_b);

        for (tid, slot) in [(SYN_IMPLANT_SLOT_4, 4.0), (SYN_IMPLANT_SLOT_6, 6.0)] {
            let mut imp = template.clone();
            imp.type_id = tid;
            imp.group_id = 300;
            imp.category_id = 20;
            imp.name = format!("Syn Implant {slot}");
            imp.default_attributes.clear();
            imp.default_attributes.insert(ATTR_IMPLANTNESS, slot);
            sde.types.insert(tid, imp);
        }
    });
    let ctx = base_ctx.with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED);

    let char_id = 90_000_001i64;
    let char2 = 90_000_002i64;
    let station_id = 60_003_760i64;
    let unauth_sess = SessionState::new(711);
    let mut sess = docked_sess(char_id, station_id);

    // non_diminishing_sp_yield returns None when containedSkillPoints == 0.0
    assert_eq!(non_diminishing_sp_yield(&ctx.sde, SYN_ZERO_YIELD_INJ), None);
    assert!(!is_non_diminishing_injector(&ctx.sde, SYN_ZERO_YIELD_INJ));

    // Unauthenticated require_character guards across RPCs
    assert_eq!(
        inject_implant(&ctx, &unauth_sess, &[PyRep::Integer(1)])
            .unwrap_err()
            .code,
        "Unauthorized"
    );
    assert_eq!(
        destroy_implant(&ctx, &unauth_sess, &[PyRep::Integer(1)])
            .unwrap_err()
            .code,
        "Unauthorized"
    );
    assert_eq!(
        get_implants(&ctx, &unauth_sess).unwrap_err().code,
        "Unauthorized"
    );
    assert_eq!(
        use_booster(&ctx, &unauth_sess, &[PyRep::Integer(1)])
            .unwrap_err()
            .code,
        "Unauthorized"
    );
    assert_eq!(
        get_boosters(&ctx, &unauth_sess).unwrap_err().code,
        "Unauthorized"
    );
    assert_eq!(
        inject_skillpoints(&ctx, &unauth_sess, &[]).unwrap_err().code,
        "Unauthorized"
    );
    assert_eq!(
        call_skill_inj_rpc(
            &ctx,
            &unauth_sess,
            "nonDiminishingInjectionMgr",
            "GetAvailableNonDiminishingInjections"
        )
        .unwrap()
        .unwrap_err()
        .code,
        "Unauthorized"
    );
    assert_eq!(
        call_skill_inj_rpc(
            &ctx,
            &unauth_sess,
            "alphaInjectorMgr",
            "GetNextAvailableInjection"
        )
        .unwrap()
        .unwrap_err()
        .code,
        "Unauthorized"
    );

    // InvalidArgument for missing itemIDs, negative item_id in destroy_implant
    assert_eq!(
        inject_implant(&ctx, &sess, &[]).unwrap_err().code,
        "InvalidArgument"
    );
    assert_eq!(
        destroy_implant(&ctx, &sess, &[]).unwrap_err().code,
        "InvalidArgument"
    );
    assert_eq!(
        destroy_implant(&ctx, &sess, &[PyRep::Integer(-5)])
            .unwrap_err()
            .code,
        "ImplantNotFound"
    );
    assert_eq!(
        use_booster(&ctx, &sess, &[]).unwrap_err().code,
        "InvalidArgument"
    );

    // Setup ship + skills + items in Cargo (in space, station_id = None) + item_custom_names fallback
    let ship_id = 880_001i64;
    let imp4_cargo_id = 880_010i64;
    let imp4_existing_item_id = 880_011i64;
    let zero_dur_id = 880_020i64;
    let boundary_b1 = 880_021i64;
    let boundary_b2 = 880_022i64;
    let exp_char2_booster = 880_023i64;
    let neg_exp_char1_booster = 880_024i64;
    let alpha_inj_1 = 880_030i64;
    let alpha_inj_2 = 880_031i64;
    let large_inj_id = 880_032i64;

    ctx.db.with_mut_snapshot(|snap| {
        snap.active_implants.remove(&char_id);
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 587,
                owner_id: char_id,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.active_ship_id = ship_id;
            ch.clone_grade = 1;
        }
        // Item slot 4 ONLY in snap.items (not in active_implants) -> triggers slot_taken_in_items
        snap.items.insert(
            imp4_existing_item_id,
            InventoryItemRecord {
                item_id: imp4_existing_item_id,
                type_id: SYN_IMPLANT_SLOT_4,
                owner_id: char_id,
                location_id: char_id,
                flag: InventoryFlag::Implant,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        // Active implant slot 6 ONLY in snap.active_implants -> tests get_implants merge + destroy without item
        snap.active_implants.insert(char_id, vec![SYN_IMPLANT_SLOT_6]);

        // Quantity-1 implant in ship Cargo
        snap.items.insert(
            imp4_cargo_id,
            InventoryItemRecord {
                item_id: imp4_cargo_id,
                type_id: SYN_IMPLANT_SLOT_4,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        // Boosters in ship Cargo
        snap.items.insert(
            zero_dur_id,
            InventoryItemRecord {
                item_id: zero_dur_id,
                type_id: SYN_ZERO_DUR_BOOSTER,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            boundary_b1,
            InventoryItemRecord {
                item_id: boundary_b1,
                type_id: SYN_BOUNDARY_BOOSTER,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            boundary_b2,
            InventoryItemRecord {
                item_id: boundary_b2,
                type_id: SYN_BOUNDARY_BOOSTER,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        // Expired booster owned by char2 (char_filter = Some(char_id) must leave it untouched!)
        snap.items.insert(
            exp_char2_booster,
            InventoryItemRecord {
                item_id: exp_char2_booster,
                type_id: SYN_BOUNDARY_BOOSTER,
                owner_id: char2,
                location_id: char2,
                flag: InventoryFlag::Booster,
                quantity: 1,
                singleton: true,
                version: 1,
                dynamic_attributes: HashMap::from([(DYN_ATTR_BOOSTER_EXPIRY_UNIX, 1_779_999_000.0)]),
                ..Default::default()
            },
        );
        // Negative expiry attribute on char_id booster -> NOT expired, NOT rolled flag
        snap.items.insert(
            neg_exp_char1_booster,
            InventoryItemRecord {
                item_id: neg_exp_char1_booster,
                type_id: SYN_NEG100_PEN_BOOSTER,
                owner_id: char_id,
                location_id: char_id,
                flag: InventoryFlag::Booster,
                quantity: 1,
                singleton: true,
                version: 1,
                dynamic_attributes: HashMap::from([(DYN_ATTR_BOOSTER_EXPIRY_UNIX, -1.0)]),
                ..Default::default()
            },
        );

        // Cooldown saved ONLY in item_custom_names (no owner_notes) -> fallback loader
        let old_ft = ctx.now_blueos_filetime_ticks() - ALPHA_INJECTOR_COOLDOWN_TICKS + 1;
        snap.item_custom_names.insert(
            ALPHA_INJECTOR_COOLDOWNS_KEY,
            serde_json::to_string(&HashMap::from([(char_id, old_ft)])).unwrap(),
        );

        // Two Daily Alpha Injectors + 1 Large Skill Injector in station hangar
        for (iid, tid) in [
            (alpha_inj_1, TYPE_DAILY_ALPHA_INJECTOR),
            (alpha_inj_2, 46375u32),
            (large_inj_id, TYPE_LARGE_SKILL_INJECTOR),
        ] {
            snap.items.insert(
                iid,
                InventoryItemRecord {
                    item_id: iid,
                    type_id: tid,
                    owner_id: char_id,
                    location_id: station_id,
                    flag: InventoryFlag::Hangar,
                    quantity: 1,
                    singleton: false,
                    version: 1,
                    ..Default::default()
                },
            );
        }
        // Set Biology to level 7 (clamps at 5 -> 2.0x)
        let skills = snap.skills.entry(char_id).or_default();
        skills.insert(
            SKILL_BIOLOGY,
            TrainedSkillRecord {
                type_id: SKILL_BIOLOGY,
                skill_points: 256_000.0,
                level: 7,
            },
        );
    });

    // Space session (station_id = None, ship_id = Some(ship_id))
    sess.station_id = None;
    sess.ship_id = Some(ship_id);

    // get_implants merges slot 4 (from items) + slot 6 (from active_implants) sorted by slot
    let PyRep::Dict(impl_dict) = get_implants(&ctx, &sess).unwrap() else {
        panic!("expected dict");
    };
    assert_eq!(impl_dict[0].0, PyRep::Integer(4));
    assert_eq!(impl_dict[1].0, PyRep::Integer(6));

    // Injecting slot 4 fails on slot_taken_in_items
    assert_eq!(
        inject_implant(&ctx, &sess, &[PyRep::Integer(imp4_cargo_id)])
            .unwrap_err()
            .code,
        "ImplantSlotOccupied"
    );

    // Destroy slot 6 via type_id only (no item in snap.items), then slot 4 via item_id
    destroy_implant(&ctx, &sess, &[PyRep::Integer(SYN_IMPLANT_SLOT_6 as i64)]).unwrap();
    destroy_implant(&ctx, &sess, &[PyRep::Integer(imp4_existing_item_id)]).unwrap();

    // Now plug qty=1 implant in-place from Cargo in space -> keeps imp4_cargo_id!
    inject_implant(&ctx, &sess, &[PyRep::Integer(imp4_cargo_id)]).unwrap();
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items[&imp4_cargo_id].flag, InventoryFlag::Implant);
        assert_eq!(snap.items[&imp4_cargo_id].location_id, char_id);
        assert!(snap.items[&imp4_cargo_id].singleton);
    });

    // purge_expired_boosters_in_snapshot with Some(char_id) preserves exp_char2_booster & neg_exp_char1_booster
    let purged_ships = ctx.db.with_mut_snapshot(|snap| {
        purge_expired_boosters_in_snapshot(snap, Some(char_id), 1_780_000_000)
    });
    assert!(purged_ships.is_empty());
    ctx.db.with_snapshot(|snap| {
        assert!(snap.items.contains_key(&exp_char2_booster));
        assert!(snap.items.contains_key(&neg_exp_char1_booster));
    });

    // active_combat_boosters_for_char omits unrolled neg_exp_char1_booster from side_effects map
    let (active_b, rolled_map) = active_combat_boosters_for_char(&ctx.db, char_id, 1_780_000_000);
    assert_eq!(active_b, vec![SYN_NEG100_PEN_BOOSTER]);
    assert!(!rolled_map.contains_key(&SYN_NEG100_PEN_BOOSTER));

    // apply_active_booster_side_effect_adjustments:
    // - unknown booster 999999 skipped
    // - base_pen == -100.0 (unreduced_factor == 0.0) skipped cleanly without NaN/inf
    // - SYN_BOUNDARY_BOOSTER missing from side_effects map -> defaults to rolled, nc_lvl=7 clamps to 5 (0.75 * -20% = -15%)
    let mut ship_attrs = HashMap::from([(MOD_SHIP_ATTR, 80.0)]);
    let skill_lvls = HashMap::from([(SKILL_NEUROTOXIN_CONTROL, 7u8)]);
    apply_active_booster_side_effect_adjustments(
        &ctx.sde,
        &mut ship_attrs,
        &skill_lvls,
        &[999_999, SYN_NEG100_PEN_BOOSTER, SYN_BOUNDARY_BOOSTER],
        &HashMap::new(),
    );
    assert!((ship_attrs[&MOD_SHIP_ATTR] - 80.0 * (0.85 / 0.80)).abs() < 1e-6);

    // use_booster error branches: Missing ItemNotFound, ItemNotOwned, base_duration_ms <= 0
    assert_eq!(
        use_booster(&ctx, &sess, &[PyRep::Integer(999_999)])
            .unwrap_err()
            .code,
        "ItemNotFound"
    );
    assert_eq!(
        use_booster(&ctx, &sess, &[PyRep::Integer(exp_char2_booster)])
            .unwrap_err()
            .code,
        "ItemNotOwned"
    );
    assert_eq!(
        use_booster(&ctx, &sess, &[PyRep::Integer(zero_dur_id)])
            .unwrap_err()
            .code,
        "ItemNotABooster"
    );

    // use_booster boundary: args[1] == 0 ignored; Biology lvl 7 clamps to lvl 5 (1000s * 2.0 = 2000s);
    // base chance == 0.10 == roll_threshold -> strict `<` does NOT roll side effect; qty == 1 consumes in-place
    let res = use_booster(&ctx, &sess, &[PyRep::Integer(boundary_b1), PyRep::Integer(0)])
        .expect("boundary booster ok");
    assert_eq!(res.dict_get("boosterID").and_then(|v| v.as_i64()), Some(boundary_b1));
    assert_eq!(res.dict_get("durationSeconds").and_then(|v| v.as_i64()), Some(2_000));
    assert_eq!(res.dict_get("sideEffectIDs"), Some(&PyRep::List(Vec::new())));

    // Slot 3 now occupied -> BoosterSlotOccupied
    assert_eq!(
        use_booster(&ctx, &sess, &[PyRep::Integer(boundary_b2)])
            .unwrap_err()
            .code,
        "BoosterSlotOccupied"
    );

    // Back in station: cooldown was 1 tick shy of elapsed -> fails, advance 1s -> GetNextAvailableInjection == 0,
    // injection succeeds AND second non-diminishing injector remains (rem_non_dim > 0 -> NO Removed notif)
    sess.station_id = Some(station_id);
    assert_eq!(
        inject_skillpoints(&ctx, &sess, &[PyRep::Integer(alpha_inj_1)])
            .unwrap_err()
            .code,
        "DailyAlphaInjectorCooldownActive"
    );
    clock.advance_secs(1);
    assert_eq!(
        call_skill_inj_rpc(&ctx, &sess, "alphaInjectorMgr", "GetNextAvailableInjection")
            .unwrap()
            .unwrap(),
        PyRep::Integer(0)
    );
    let c1_uid = ctx.db.with_snapshot(|s| s.characters[&char_id].user_id);
    ctx.db
        .acquire_online_lock_with_computer_hash(c1_uid, 91_101, Some(char_id), None, true, 100)
        .expect("bring injector pilot online");
    let _ = ctx.drain_notifications_for_char(char_id);
    let sp1 = inject_skillpoints(&ctx, &sess, &[PyRep::Integer(alpha_inj_1)])
        .unwrap()
        .as_f64()
        .unwrap();
    assert_eq!(sp1, 50_000.0);
    let notifs = ctx.drain_notifications_for_char(char_id);
    assert!(notifs
        .iter()
        .any(|n| n.event == "OnNonDiminishingInjectionsUsed"));
    assert!(!notifs
        .iter()
        .any(|n| n.event == "OnNonDiminishingInjectionsRemoved"));

    // GetAvailableNonDiminishingInjections sees alpha_inj_2 (46375 -> qty 1)
    let PyRep::Dict(avail) = call_skill_inj_rpc(
        &ctx,
        &sess,
        "nonDiminishingInjectionMgr",
        "GetAvailableNonDiminishingInjections",
    )
    .unwrap()
    .unwrap() else {
        panic!("expected dict");
    };
    assert_eq!(avail, vec![(PyRep::Integer(46375), PyRep::Integer(1))]);

    // Remove alpha_inj_2 so large_inj_id is unambiguously found when itemID arg is omitted
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.remove(&alpha_inj_2);
    });
    let large_sp = inject_skillpoints(&ctx, &sess, &[]).unwrap().as_f64().unwrap();
    assert!(large_sp >= 150_000.0);

    // CheckAndSendNotifications works for authenticated session
    assert_eq!(
        call_skill_inj_rpc(&ctx, &sess, "skillMgr2", "CheckAndSendNotifications")
            .unwrap()
            .unwrap(),
        PyRep::None
    );

    // Missing character in snapshot -> CharacterNotFound
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.remove(&char_id);
    });
    assert_eq!(
        inject_skillpoints(&ctx, &sess, &[]).unwrap_err().code,
        "CharacterNotFound"
    );
}

#[tokio::test]
async fn mutants_cov_booster_purge_ballpark_sync_implant_version_destroy_and_cargo_sp() {
    use eve_db::{InventoryFlag, InventoryItemRecord, ServerClock};
    use eve_sde::{DogmaEffectType, EffectModifierSpec};

    let imp_slot1_tid = 992_001u32;
    let imp_shield_tid = 992_002u32;
    let bst_normal_tid = 992_003u32;
    let bst_clone_tid = 992_004u32;
    let shield_eff_id = 60_921u16;
    let base_now = 1_800_040_000u64;

    let (ctx, clock) = make_ctx_with_sde(base_now, |sde: &mut SdeRegistry| {
        let base_t = sde.types.values().next().unwrap().clone();
        sde.effects.insert(
            shield_eff_id,
            DogmaEffectType {
                effect_id: shield_eff_id,
                name: "testImplantShieldBoost".into(),
                effect_category: 0,
                is_offensive: false,
                is_assistance: false,
                published: true,
                modifiers: vec![EffectModifierSpec {
                    domain: "shipID".into(),
                    func: "ItemModifier".into(),
                    modified_attribute_id: eve_sde::attr_ids::SHIELD_CAPACITY,
                    modifying_attribute_id: 337,
                    operator: 2,
                    group_id: None,
                    skill_type_id: None,
                    effect_id: Some(shield_eff_id),
                }],
                ..Default::default()
            },
        );
        let mut t1 = base_t.clone();
        t1.type_id = imp_slot1_tid;
        t1.category_id = 20;
        t1.group_id = 300;
        t1.default_attributes.clear();
        t1.default_attributes.insert(ATTR_IMPLANTNESS, 1.0);
        t1.effect_ids.clear();
        sde.types.insert(imp_slot1_tid, t1);

        let mut t2 = base_t.clone();
        t2.type_id = imp_shield_tid;
        t2.category_id = 20;
        t2.group_id = 300;
        t2.default_attributes.clear();
        t2.default_attributes.insert(ATTR_IMPLANTNESS, 2.0);
        t2.default_attributes.insert(337, 50.0);
        t2.effect_ids = vec![shield_eff_id];
        sde.types.insert(imp_shield_tid, t2);

        let mut b1 = base_t.clone();
        b1.type_id = bst_normal_tid;
        b1.category_id = 20;
        b1.group_id = 303;
        b1.default_attributes.clear();
        b1.default_attributes.insert(ATTR_BOOSTERNESS, 1.0);
        b1.default_attributes.insert(ATTR_BOOSTER_DURATION_MS, 60_000.0);
        b1.effect_ids.clear();
        sde.types.insert(bst_normal_tid, b1);

        let mut b2 = base_t;
        b2.type_id = bst_clone_tid;
        b2.category_id = 20;
        b2.group_id = 303;
        b2.default_attributes.clear();
        b2.default_attributes.insert(ATTR_BOOSTERNESS, 2.0);
        b2.default_attributes.insert(ATTR_BOOSTER_DURATION_MS, 60_000.0);
        b2.default_attributes.insert(ATTR_FOLLOWS_JUMP_CLONES, 1.0);
        b2.effect_ids.clear();
        sde.types.insert(bst_clone_tid, b2);
    });

    let c1 = 910_001i64;
    let c2 = 910_002i64;
    let ship1 = 910_101i64;
    let ship2 = 910_102i64;
    let sys_id = 30_000_142u32;
    let station_id = 60_003_760i64;

    let b1_exp_id = 910_201i64;
    let b1_live_id = 910_202i64;
    let b2_exp_id = 910_203i64;
    let imp_split_id = 910_204i64;
    let imp_active_only_tid = imp_slot1_tid;
    let imp_item_only_id = 910_205i64;
    let ssi_cargo_id = 910_206i64;

    ctx.db.with_mut_snapshot(|snap| {
        if !snap.accounts.values().any(|a| a.user_id == 2) {
            if let Some(mut acct2) = snap.accounts.values().next().cloned() {
                acct2.user_id = 2;
                acct2.username = "implant_c2_uid2".into();
                snap.accounts.insert(acct2.username.clone(), acct2);
            }
        }
        let base_ch = snap.characters.values().next().cloned().unwrap();
        for &(cid, uid, sid, docked) in &[(c1, base_ch.user_id, ship1, true), (c2, 2u64, ship2, false)] {
            let mut ch = base_ch.clone();
            ch.character_id = cid;
            ch.user_id = uid;
            ch.name = format!("ImplantChar-{cid}");
            ch.solar_system_id = sys_id;
            ch.station_id = if docked { Some(station_id) } else { None };
            ch.structure_id = None;
            ch.active_ship_id = sid;
            ch.free_skill_points = 1_000.0;
            snap.characters.insert(cid, ch);

            snap.items.insert(
                sid,
                InventoryItemRecord {
                    item_id: sid,
                    type_id: 587,
                    owner_id: cid,
                    location_id: if docked { station_id } else { sys_id as i64 },
                    flag: if docked { InventoryFlag::Hangar } else { InventoryFlag::None },
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
        }
        // c2 has active shield capacity implant (+50% shield) so expected_c2 != expected_c1
        snap.active_implants.insert(c2, vec![imp_shield_tid]);

        let mk_bst = |id: i64, owner: i64, tid: u32, exp: f64| {
            let mut r = InventoryItemRecord {
                item_id: id,
                type_id: tid,
                owner_id: owner,
                location_id: owner,
                flag: InventoryFlag::Booster,
                quantity: 1,
                singleton: true,
                ..Default::default()
            };
            r.dynamic_attributes.insert(DYN_ATTR_BOOSTER_EXPIRY_UNIX, exp);
            r.dynamic_attributes.insert(DYN_ATTR_BOOSTER_DURATION_SECS, 60.0);
            r
        };
        snap.items.insert(b1_exp_id, mk_bst(b1_exp_id, c1, bst_normal_tid, (base_now - 10) as f64));
        snap.items.insert(b1_live_id, mk_bst(b1_live_id, c1, bst_clone_tid, (base_now + 500) as f64));
        snap.items.insert(b2_exp_id, mk_bst(b2_exp_id, c2, bst_normal_tid, (base_now - 5) as f64));

        snap.items.insert(
            imp_split_id,
            InventoryItemRecord {
                item_id: imp_split_id,
                type_id: imp_slot1_tid,
                owner_id: c1,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 2,
                singleton: false,
                version: 5,
                ..Default::default()
            },
        );
        snap.items.insert(
            ssi_cargo_id,
            InventoryItemRecord {
                item_id: ssi_cargo_id,
                type_id: TYPE_SMALL_SKILL_INJECTOR,
                owner_id: c2,
                location_id: ship2,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                singleton: false,
                ..Default::default()
            },
        );
    });

    // 1. purge_expired_boosters_in_snapshot_with_items (scoped to c1, preserves live and c2)
    ctx.db.with_mut_snapshot(|snap| {
        let (ships, removed) = purge_expired_boosters_in_snapshot_with_items(snap, Some(c1), base_now);
        assert_eq!(ships, vec![ship1]);
        assert_eq!(removed.len(), 1);
        assert_eq!(removed[0].0, c1);
        assert_eq!(removed[0].1.item_id, b1_exp_id);
        assert!(!snap.items.contains_key(&b1_exp_id));
        assert!(snap.items.contains_key(&b1_live_id));
        assert!(snap.items.contains_key(&b2_exp_id));

        // Non-expired returns empty
        let (ships_none, rem_none) = purge_expired_boosters_in_snapshot_with_items(snap, Some(c1), base_now);
        assert!(ships_none.is_empty());
        assert!(rem_none.is_empty());
    });

    // 2. purge_expired_boosters_all invalidates ship_fittings, notifies owner, and syncs ballpark stats to exact owner (c2 != c1)
    let sys_h = ctx.systems.get_or_spawn(sys_id);
    sys_h
        .spawn_ball(eve_destiny::Ball {
            id: ship2,
            type_id: 587,
            position: glam::DVec3::ZERO,
            velocity: glam::DVec3::ZERO,
            max_velocity: 100.0,
            inertia: 2.0,
            mass: 1_000_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: eve_destiny::MovementMode::Stop,
        })
        .await
        .unwrap();
    let _ = sys_h
        .set_combat_state(
            ship2,
            eve_destiny::CombatState {
                shield_hp: 1.0,
                max_shield_hp: 1.0,
                armor_hp: 1.0,
                max_armor_hp: 1.0,
                structure_hp: 1.0,
                max_structure_hp: 1.0,
                capacitor: 1.0,
                max_capacitor: 1.0,
                ..Default::default()
            },
        )
        .await;
    let _ = ctx.get_or_rebuild_ship_fitting(c2, ship2).unwrap();
    assert!(ctx.ship_fittings.read().unwrap().contains_key(&ship2));
    ctx.db
        .acquire_online_lock(2, 910_002, Some(c2), false, base_now)
        .unwrap();
    ctx.pending_notifications.write().unwrap().remove(&c2);

    purge_expired_boosters_all(&ctx, base_now);
    assert!(ctx
        .ship_fittings
        .read()
        .unwrap()
        .get(&ship2)
        .is_some_and(|f| f.boosters.is_empty()));
    assert!(!ctx.db.with_snapshot(|s| s.items.contains_key(&b2_exp_id)));
    assert!(ctx.pending_notifications.read().unwrap().get(&c2).is_some_and(|v| !v.is_empty()));

    let expected_c1 = ctx.build_ship_combat_state(c1, ship1, 587);
    let expected_c2 = ctx.build_ship_combat_state(c2, ship2, 587);
    assert!((expected_c2.max_shield_hp - expected_c1.max_shield_hp).abs() > 1.0);
    let bp_cs = sys_h.ballpark.read().unwrap().combat_states.get(&ship2).cloned().unwrap();
    assert!((bp_cs.max_shield_hp - expected_c2.max_shield_hp).abs() < 1e-3);

    // 3. purge_expired_boosters_in_snapshot returns invalidated ships directly when unscoped
    clock.advance_secs(600);
    let future_now = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        let purged_ships = purge_expired_boosters_in_snapshot(snap, None, future_now);
        assert_eq!(purged_ships, vec![ship1]);
        assert!(!snap.items.contains_key(&b1_live_id));
    });

    // 4. sync_clone_jump_implants_and_boosters preserves ATTR_FOLLOWS_JUMP_CLONES (1916=1.0), removes non-1916, syncs implant items
    ctx.db.with_mut_snapshot(|snap| {
        let now_s = future_now;
        let mk_bst = |id: i64, tid: u32| {
            let mut r = InventoryItemRecord {
                item_id: id,
                type_id: tid,
                owner_id: c1,
                location_id: c1,
                flag: InventoryFlag::Booster,
                quantity: 1,
                singleton: true,
                ..Default::default()
            };
            r.dynamic_attributes.insert(DYN_ATTR_BOOSTER_EXPIRY_UNIX, (now_s + 1_000) as f64);
            r
        };
        snap.items.insert(910_301, mk_bst(910_301, bst_normal_tid));
        snap.items.insert(910_302, mk_bst(910_302, bst_clone_tid));
        snap.items.insert(
            910_303,
            InventoryItemRecord {
                item_id: 910_303,
                type_id: imp_shield_tid,
                owner_id: c1,
                location_id: c1,
                flag: InventoryFlag::Implant,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.active_implants.insert(c1, vec![imp_slot1_tid]);
        sync_clone_jump_implants_and_boosters(&ctx.sde, snap, c1);
        assert!(!snap.items.contains_key(&910_301));
        assert!(snap.items.contains_key(&910_302));
        assert!(!snap.items.contains_key(&910_303));
        assert!(snap.items.values().any(|i| i.owner_id == c1 && i.flag == InventoryFlag::Implant && i.type_id == imp_slot1_tid));
    });

    // 5. inject_implant split path bumps src.version += 1 (5 -> 6) and destroy_implant covers both stores + missing
    let s1 = {
        let mut s = docked_sess(c1, station_id);
        s.ship_id = Some(ship1);
        s
    };

    // Clear c1 active_implants slot 1 so inject_implant succeeds cleanly
    ctx.db.with_mut_snapshot(|snap| {
        snap.active_implants.remove(&c1);
        snap.items.retain(|_, i| !(i.owner_id == c1 && i.flag == InventoryFlag::Implant));
    });
    inject_implant(&ctx, &s1, &[PyRep::Integer(imp_split_id)]).unwrap();
    ctx.db.with_snapshot(|snap| {
        let src = snap.items.get(&imp_split_id).unwrap();
        assert_eq!(src.quantity, 1);
        assert_eq!(src.version, 6);
    });

    // destroy_implant positive non-existent ID -> ImplantNotFound
    assert_eq!(destroy_implant(&ctx, &s1, &[PyRep::Integer(999_998)]).unwrap_err().code, "ImplantNotFound");
    // active_implants-only destroy succeeds
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.retain(|_, i| !(i.owner_id == c1 && i.flag == InventoryFlag::Implant));
        snap.active_implants.insert(c1, vec![imp_active_only_tid]);
    });
    assert_eq!(destroy_implant(&ctx, &s1, &[PyRep::Integer(imp_active_only_tid as i64)]).unwrap(), PyRep::None);
    // items-only destroy by type_id succeeds
    ctx.db.with_mut_snapshot(|snap| {
        snap.active_implants.remove(&c1);
        snap.items.insert(
            imp_item_only_id,
            InventoryItemRecord {
                item_id: imp_item_only_id,
                type_id: imp_shield_tid,
                owner_id: c1,
                location_id: c1,
                flag: InventoryFlag::Implant,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });
    assert_eq!(destroy_implant(&ctx, &s1, &[PyRep::Integer(imp_shield_tid as i64)]).unwrap(), PyRep::None);

    // 6. get_boosters returns exact booster dict keys before expiry and purges after clock advance
    let b_hangar_id = 910_350i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.retain(|_, i| !(i.owner_id == c1 && i.flag == InventoryFlag::Booster));
        snap.items.insert(
            b_hangar_id,
            InventoryItemRecord {
                item_id: b_hangar_id,
                type_id: bst_normal_tid,
                owner_id: c1,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });
    use_booster(&ctx, &s1, &[PyRep::Integer(b_hangar_id)]).unwrap();
    let PyRep::Dict(entries) = get_boosters(&ctx, &s1).unwrap() else {
        panic!("expected booster Dict");
    };
    assert_eq!(entries.len(), 1);
    let b_val = &entries[0].1;
    assert_eq!(b_val.dict_get("boosterID"), Some(&PyRep::Integer(b_hangar_id)));
    assert_eq!(b_val.dict_get("boosterSlot"), Some(&PyRep::Integer(1)));
    assert!(b_val.dict_get("durationSeconds").is_some());
    assert!(b_val.dict_get("expiryTime").is_some());
    assert!(b_val.dict_get("sideEffectIDs").is_some());

    clock.advance_secs(120);
    let PyRep::Dict(empty_after) = get_boosters(&ctx, &s1).unwrap() else { panic!() };
    assert!(empty_after.is_empty());

    // 7. inject_skillpoints from undocked active ship Cargo hold updates free_skill_points += y
    let mut s2 = SessionState::default();
    s2.user_id = Some(c2 as u64);
    s2.character_id = Some(c2);
    s2.solar_system_id = Some(sys_id);
    s2.station_id = None;
    s2.structure_id = None;
    s2.ship_id = Some(ship2);

    let before_free = ctx.db.with_snapshot(|s| s.characters[&c2].free_skill_points);
    let sp_added = inject_skillpoints(&ctx, &s2, &[PyRep::Integer(ssi_cargo_id)])
        .unwrap()
        .as_f64()
        .unwrap();
    assert!(sp_added > 0.0);
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.items.contains_key(&ssi_cargo_id));
        assert!((snap.characters[&c2].free_skill_points - (before_free + sp_added)).abs() < 1e-6);
    });
}

