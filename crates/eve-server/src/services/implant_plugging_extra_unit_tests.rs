//! Deep branch-coverage and mutation-killing unit tests for `services::implant_plugging`.

#![forbid(unsafe_code)]

use super::implant_plugging::*;
use crate::router::ServerContext;
use eve_db::clock::VirtualServerClock;
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
