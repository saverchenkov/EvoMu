#![forbid(unsafe_code)]

use super::*;
use eve_db::{InventoryFlag, InventoryItemRecord, VirtualServerClock, WorldRepository};
use eve_dogma::{FittedModule, ModuleState, ShipFittingEngine};
use eve_sde::{InvType, SdeRegistry};
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

fn custom_inv_type(
    sde: &SdeRegistry,
    type_id: u32,
    group_id: u32,
    category_id: u32,
    name: &str,
    attrs: &[(u16, f64)],
    effects: &[u16],
) -> InvType {
    let mut t = sde.types.get(&587).expect("seed type").clone();
    t.type_id = type_id;
    t.group_id = group_id;
    t.category_id = category_id;
    t.name = name.into();
    t.default_attributes.clear();
    for &(k, v) in attrs {
        t.default_attributes.insert(k, v);
    }
    t.effect_ids = effects.to_vec();
    t
}

fn test_ctx_with_custom<F: FnOnce(&mut SdeRegistry)>(mutate_sde: F) -> ServerContext {
    let mut sde = (*shared_sde()).clone();
    mutate_sde(&mut sde);
    let db = Arc::new(WorldRepository::with_demo_seed());
    let ctx = ServerContext::with_sde_and_repository(Arc::new(sde), db);
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_800_000_000)));
    ctx
}

#[test]
fn overload_multiplier_avoids_double_application_for_category5_effects() {
    let ctx = test_ctx_with_custom(|sde| {
        let mut eff = sde.effects.values().next().expect("seed effect").clone();
        eff.effect_id = 65_001;
        eff.name = "overloadTestEffect".into();
        eff.effect_category = 5;
        sde.effects.insert(65_001, eff);
    });

    let plain_mod = custom_inv_type(&ctx.sde, 990_101, 62, 7, "Plain Armor Repairer", &[], &[11]);
    let cat5_mod = custom_inv_type(&ctx.sde, 990_102, 62, 7, "Cat5 Armor Repairer", &[], &[11, 65_001]);

    assert!(!has_category5_overload(&plain_mod, &ctx.sde));
    assert!(has_category5_overload(&cat5_mod, &ctx.sde));

    assert!((compute_overload_multiplier(false, &plain_mod, &ctx.sde) - 1.0).abs() < 1e-9);
    assert!((compute_overload_multiplier(true, &plain_mod, &ctx.sde) - 1.20).abs() < 1e-9);
    assert!((compute_overload_multiplier(true, &cat5_mod, &ctx.sde) - 1.0).abs() < 1e-9);
}

#[test]
fn prepare_ancillary_module_handles_asb_and_aar_fueled_and_unfueled_states() {
    let ctx = test_ctx_with_custom(|sde| {
        sde.types.insert(
            990_201,
            custom_inv_type(
                sde,
                990_201,
                1156,
                7,
                "Medium Ancillary Shield Booster",
                &[(6, 120.0), (73, 4_000.0)],
                &[13],
            ),
        );
        sde.types.insert(
            990_202,
            custom_inv_type(
                sde,
                990_202,
                1199,
                7,
                "Medium Ancillary Armor Repairer",
                &[(6, 80.0), (56, 4.0), (73, 5_000.0), (84, 150.0)],
                &[11],
            ),
        );
        sde.types.insert(
            990_203,
            custom_inv_type(
                sde,
                990_203,
                1698,
                7,
                "Medium Ancillary Remote Armor Repairer",
                &[(6, 90.0), (56, 4.0), (73, 5_000.0), (84, 120.0)],
                &[12],
            ),
        );
    });

    let char_id = 95_000_001;
    let ship_id = 1_000_950_001;
    let asb_id = 1_000_950_002;
    let cap_charge_id = 1_000_950_003;
    let aar_id = 1_000_950_004;
    let paste_id = 1_000_950_005;
    let raar_id = 1_000_950_006;

    let mut fit = ShipFittingEngine::from_sde(ship_id, 587, &ctx.sde);
    fit.modules.insert(
        asb_id,
        FittedModule {
            item_id: asb_id,
            type_id: 990_201,
            flag_id: InventoryFlag::MedSlot0 as u16,
            state: ModuleState::Active,
            charge_type_id: Some(11283),
            charge_quantity: 1,
            damage_hp: 0.0,
            max_hp: 40.0,
            dynamic_attribute_overrides: HashMap::new(),
        },
    );
    fit.modules.insert(
        aar_id,
        FittedModule {
            item_id: aar_id,
            type_id: 990_202,
            flag_id: InventoryFlag::LoSlot0 as u16,
            state: ModuleState::Active,
            charge_type_id: Some(28668),
            charge_quantity: 4,
            damage_hp: 0.0,
            max_hp: 40.0,
            dynamic_attribute_overrides: HashMap::new(),
        },
    );
    fit.modules.insert(
        raar_id,
        FittedModule {
            item_id: raar_id,
            type_id: 990_203,
            flag_id: InventoryFlag::HiSlot0 as u16,
            state: ModuleState::Active,
            charge_type_id: None,
            charge_quantity: 0,
            damage_hp: 0.0,
            max_hp: 40.0,
            dynamic_attribute_overrides: HashMap::new(),
        },
    );
    fit.recalculate_stats(&ctx.sde);

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            cap_charge_id,
            InventoryItemRecord {
                item_id: cap_charge_id,
                type_id: 11283,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::MedSlot0,
                quantity: 1,
                singleton: false,
                ..Default::default()
            },
        );
        snap.items.insert(
            paste_id,
            InventoryItemRecord {
                item_id: paste_id,
                type_id: 28668,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::LoSlot0,
                quantity: 4,
                singleton: false,
                ..Default::default()
            },
        );
    });

    let asb_type = ctx.sde.types.get(&990_201).unwrap();
    // 1st ASB cycle consumes the single cap booster charge -> 0.0 cap_need, charge removed
    let prep1 = prepare_ancillary_module(
        &ctx,
        ship_id,
        asb_id,
        InventoryFlag::MedSlot0 as u16,
        asb_type,
        &fit,
        false,
        120.0,
        0.0,
        0.0,
        1.0,
    );
    assert!(prep1.is_asb);
    assert!((prep1.cap_need - 0.0).abs() < 1e-6);
    assert!((prep1.asb_fallback_cap_need - 120.0).abs() < 1e-6);
    assert!(ctx.db.with_snapshot(|s| !s.items.contains_key(&cap_charge_id)));

    // 2nd ASB cycle is unfueled -> falls back to asb_fallback_cap_need (120.0)
    let prep2 = prepare_ancillary_module(
        &ctx,
        ship_id,
        asb_id,
        InventoryFlag::MedSlot0 as u16,
        asb_type,
        &fit,
        false,
        120.0,
        0.0,
        0.0,
        1.0,
    );
    assert!((prep2.cap_need - 120.0).abs() < 1e-6);

    // 1st AAR cycle consumes 4 Nanite Repair Paste -> 3x armor repair (450.0)
    let aar_type = ctx.sde.types.get(&990_202).unwrap();
    let aar_fueled = prepare_ancillary_module(
        &ctx,
        ship_id,
        aar_id,
        InventoryFlag::LoSlot0 as u16,
        aar_type,
        &fit,
        false,
        80.0,
        150.0,
        0.0,
        1.0,
    );
    assert!(aar_fueled.is_aar);
    assert_eq!(aar_fueled.aar_paste_rate, 4);
    assert!((aar_fueled.armor_repair_amount - 450.0).abs() < 1e-6);

    // 2nd AAR cycle is unfueled -> 1x armor repair (150.0)
    let aar_unfueled = prepare_ancillary_module(
        &ctx,
        ship_id,
        aar_id,
        InventoryFlag::LoSlot0 as u16,
        aar_type,
        &fit,
        false,
        80.0,
        150.0,
        0.0,
        1.0,
    );
    assert!((aar_unfueled.armor_repair_amount - 150.0).abs() < 1e-6);

    // Unfueled Remote Ancillary Armor Repairer (group 1698) -> 1x remote_armor_amount
    let raar_type = ctx.sde.types.get(&990_203).unwrap();
    let raar_prep = prepare_ancillary_module(
        &ctx,
        ship_id,
        raar_id,
        InventoryFlag::HiSlot0 as u16,
        raar_type,
        &fit,
        false,
        90.0,
        0.0,
        120.0,
        1.0,
    );
    assert!(raar_prep.is_aar);
    assert!((raar_prep.remote_armor_amount - 120.0).abs() < 1e-6);
}

#[tokio::test]
async fn handle_active_overload_toggle_and_nanite_lockout_check() {
    let ctx = test_ctx_with_custom(|_| {});
    let char_id = 95_000_010;
    let sys_id = 30_000_142;
    let ship_id = 1_000_950_010;
    let mod_id = 1_000_950_011;
    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().cloned().expect("seed char");
        ch.character_id = char_id;
        ch.user_id = char_id as u64;
        snap.characters.insert(char_id, ch);
        snap.accounts.insert(
            format!("user_{char_id}"),
            eve_db::AccountRecord {
                user_id: char_id as u64,
                username: format!("user_{char_id}"),
                password_token: String::new(),
                role: 0,
                banned_until_unix: None,
                ban_reason: None,
            },
        );
    });
    let _ = ctx
        .db
        .acquire_online_lock(char_id as u64, char_id as u64, Some(char_id), true, ctx.now_unix_secs());

    let sys = ctx.systems.get_or_spawn(sys_id);
    assert!(check_nanite_repair_overheat_lockout(&sys, ship_id, mod_id, "Overload", ModuleState::Online).is_ok());

    let mut fit = ShipFittingEngine::from_sde(ship_id, 587, &ctx.sde);
    fit.modules.insert(
        mod_id,
        FittedModule {
            item_id: mod_id,
            type_id: 439,
            flag_id: InventoryFlag::MedSlot0 as u16,
            state: ModuleState::Online,
            charge_type_id: None,
            charge_quantity: 0,
            damage_hp: 0.0,
            max_hp: 40.0,
            dynamic_attribute_overrides: HashMap::new(),
        },
    );
    fit.recalculate_stats(&ctx.sde);

    let rep_pre = handle_active_overload_toggle(&ctx, &sys, char_id, ship_id, mod_id, fit.clone(), false).await;
    assert_eq!(rep_pre.dict_get("moduleID").and_then(|v| v.as_i64()), Some(mod_id));
    assert_eq!(rep_pre.dict_get("overloaded").and_then(|v| v.as_bool()), Some(true));
    assert!(ctx
        .drain_notifications_for_char(char_id)
        .iter()
        .any(|n| n.event == "OnHeatAdded"));
    assert_eq!(
        ctx.get_or_rebuild_ship_fitting(char_id, ship_id).unwrap().modules[&mod_id].state,
        ModuleState::Overloaded
    );

    let rep_active = handle_active_overload_toggle(&ctx, &sys, char_id, ship_id, mod_id, fit, true).await;
    assert_eq!(rep_active.dict_get("overloaded").and_then(|v| v.as_bool()), Some(true));
}

#[tokio::test]
async fn ancillary_overload_lockout_and_rack_heat_mutants() {
    use crate::rng::{MechanicRng, TEST_MODE_DEFAULT_SEED};
    use crate::services::inventory_fitting::fitting_mining_lasers::compute_nanite_repair_params;
    use crate::services::tactical_module_mechanics::{
        apply_overheat_rack_and_damage_on_cycle, deduct_paste_from_cargo_in_snap,
        evaluate_ship_thermodynamics, refund_paste_to_cargo_capped_in_snap,
        ship_free_cargo_m3_in_snap, step_rack_heat_per_tick, RACK_HEAT_ATTR_IDS,
    };
    use crate::system_actor::{ActiveModuleCycle, ActiveNaniteRepair};
    use eve_db::TrainedSkillRecord;

    let ctx = test_ctx_with_custom(|sde| {
        sde.types.insert(
            990_301,
            custom_inv_type(sde, 990_301, 1697, 7, "Capital ASB", &[(6, 100.0), (73, 4_000.0)], &[13]),
        );
        sde.types.insert(
            990_302,
            custom_inv_type(sde, 990_302, 1698, 7, "Remote AAR", &[(6, 90.0), (56, 2.6), (73, 5_000.0), (84, 200.0)], &[13]),
        );
        sde.types.insert(
            990_303,
            custom_inv_type(sde, 990_303, 1199, 7, "Clamp AAR", &[(6, 50.0), (56, 0.0), (84, 100.0)], &[13]),
        );
        sde.types.insert(
            990_304,
            custom_inv_type(sde, 990_304, 1199, 7, "Default Rate AAR", &[(6, 50.0), (84, 100.0)], &[13]),
        );
        sde.types.insert(
            990_305,
            custom_inv_type(
                sde,
                990_305,
                62,
                7,
                "Plain Thermal Laser",
                &[(6, 30.0), (73, 1_000.0), (1180, 0.05), (1211, 20.0), (attr_ids::HP_STRUCT, 40.0)],
                &[11],
            ),
        );
    })
    .with_seeded_rng(TEST_MODE_DEFAULT_SEED);

    // 1. Overloaded non-cat5 ancillary AAR vs non-ancillary plain module & group 1697/1698 & paste rounding/clamping
    let char_id = 95_000_077;
    let ship_id = 1_000_950_077;
    let mod_cap_asb = 1_000_950_078;
    let mod_raar = 1_000_950_079;
    let mod_clamp_aar = 1_000_950_080;
    let mod_default_aar = 1_000_950_081;
    let mod_plain = 1_000_950_082;
    let paste_raar = 1_000_950_083;
    let paste_clamp = 1_000_950_084;
    let cap_charge = 1_000_950_085;

    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().cloned().expect("seed char");
        ch.character_id = char_id;
        ch.user_id = char_id as u64;
        snap.characters.insert(char_id, ch);
        let mk = |item_id, type_id, flag, qty, singleton| InventoryItemRecord {
            item_id,
            type_id,
            owner_id: char_id,
            location_id: ship_id,
            flag,
            quantity: qty,
            singleton,
            version: 1,
            dynamic_attributes: HashMap::new(),
            source_mutaplasmid_id: None,
            insured_payout_isk: 0.0,
            is_blueprint_copy: false,
            blueprint_runs_remaining: None,
        };
        snap.items.insert(ship_id, InventoryItemRecord {
            location_id: 60_003_760,
            ..mk(ship_id, 587, InventoryFlag::Hangar, 1, true)
        });
        snap.items.insert(mod_cap_asb, mk(mod_cap_asb, 990_301, InventoryFlag::MedSlot0, 1, true));
        snap.items.insert(cap_charge, mk(cap_charge, 32014, InventoryFlag::MedSlot0, 1, false));
        snap.items.insert(mod_raar, mk(mod_raar, 990_302, InventoryFlag::HiSlot0, 1, true));
        // Exact rounded paste threshold: round(2.6) = 3; seed 3 so fueled succeeds and removes stack
        snap.items.insert(paste_raar, mk(paste_raar, 28668, InventoryFlag::HiSlot0, 3, false));
        snap.items.insert(mod_clamp_aar, mk(mod_clamp_aar, 990_303, InventoryFlag::LoSlot0, 1, true));
        // Quantity 0 fails >= 1 check after max(1.0) clamp
        snap.items.insert(paste_clamp, mk(paste_clamp, 28668, InventoryFlag::LoSlot0, 0, false));
        snap.items.insert(mod_default_aar, mk(mod_default_aar, 990_304, InventoryFlag::LoSlot1, 1, true));
        snap.items.insert(mod_plain, mk(mod_plain, 990_305, InventoryFlag::HiSlot1, 1, true));
    });

    let mut fit = ShipFittingEngine::from_sde(ship_id, 587, &ctx.sde);
    for (mid, tid, flag, charge) in [
        (mod_cap_asb, 990_301, InventoryFlag::MedSlot0 as u16, Some(32014)),
        (mod_raar, 990_302, InventoryFlag::HiSlot0 as u16, Some(28668)),
        (mod_clamp_aar, 990_303, InventoryFlag::LoSlot0 as u16, Some(28668)),
        (mod_default_aar, 990_304, InventoryFlag::LoSlot1 as u16, None),
        (mod_plain, 990_305, InventoryFlag::HiSlot1 as u16, None),
    ] {
        fit.modules.insert(
            mid,
            FittedModule {
                item_id: mid,
                type_id: tid,
                flag_id: flag,
                state: ModuleState::Online,
                charge_type_id: charge,
                charge_quantity: 1,
                damage_hp: 0.0,
                max_hp: 40.0,
                dynamic_attribute_overrides: HashMap::new(),
            },
        );
    }
    fit.recalculate_stats(&ctx.sde);

    // Group 1697 fueled -> cap_need 0.0; second call unfueled -> fallback cap_need 100.0
    let p_asb_fueled = prepare_ancillary_module(
        &ctx, ship_id, mod_cap_asb, InventoryFlag::MedSlot0 as u16,
        ctx.sde.types.get(&990_301).unwrap(), &fit, false, 100.0, 0.0, 0.0, 1.0,
    );
    assert!(p_asb_fueled.is_asb && (p_asb_fueled.cap_need - 0.0).abs() < 1e-6);
    assert!((p_asb_fueled.asb_fallback_cap_need - 100.0).abs() < 1e-6);
    let p_asb_unfueled = prepare_ancillary_module(
        &ctx, ship_id, mod_cap_asb, InventoryFlag::MedSlot0 as u16,
        ctx.sde.types.get(&990_301).unwrap(), &fit, false, 100.0, 0.0, 0.0, 1.0,
    );
    assert!((p_asb_unfueled.cap_need - 100.0).abs() < 1e-6);

    // Group 1698 fueled: paste rate round(2.6)=3, remote_armor_amount = 3.0 * initial_remote_armor (armor_repair_amount untouched)
    let p_raar = prepare_ancillary_module(
        &ctx, ship_id, mod_raar, InventoryFlag::HiSlot0 as u16,
        ctx.sde.types.get(&990_302).unwrap(), &fit, false, 90.0, 12.0, 200.0, 1.0,
    );
    assert!(p_raar.is_aar && !p_raar.is_asb);
    assert_eq!(p_raar.aar_paste_rate, 3);
    assert!((p_raar.remote_armor_amount - 600.0).abs() < 1e-6);
    assert!((p_raar.armor_repair_amount - 12.0).abs() < 1e-6);
    assert!(ctx.db.with_snapshot(|s| !s.items.contains_key(&paste_raar)));

    // Group 1199 clamp 0.0 -> max(1.0) = 1, qty=0 unfueled -> base * overload_mult (1.20) = 120.0
    let p_clamp = prepare_ancillary_module(
        &ctx, ship_id, mod_clamp_aar, InventoryFlag::LoSlot0 as u16,
        ctx.sde.types.get(&990_303).unwrap(), &fit, true, 50.0, 100.0, 0.0, 1.20,
    );
    assert_eq!(p_clamp.aar_paste_rate, 1);
    assert!((p_clamp.armor_repair_amount - 120.0).abs() < 1e-6);

    // Missing attr 56 defaults to 4
    let p_def = prepare_ancillary_module(
        &ctx, ship_id, mod_default_aar, InventoryFlag::LoSlot1 as u16,
        ctx.sde.types.get(&990_304).unwrap(), &fit, false, 50.0, 100.0, 0.0, 1.0,
    );
    assert_eq!(p_def.aar_paste_rate, 4);

    // Non-ancillary module returns inputs verbatim
    let p_plain = prepare_ancillary_module(
        &ctx, ship_id, mod_plain, InventoryFlag::HiSlot1 as u16,
        ctx.sde.types.get(&990_305).unwrap(), &fit, false, 30.0, 77.0, 55.0, 1.0,
    );
    assert!(!p_plain.is_asb && !p_plain.is_aar);
    assert!((p_plain.cap_need - 30.0).abs() < 1e-6);
    assert!((p_plain.armor_repair_amount - 77.0).abs() < 1e-6);
    assert!((p_plain.remote_armor_amount - 55.0).abs() < 1e-6);
    assert!((p_plain.aar_base_armor_repair - 0.0).abs() < 1e-6);

    // 2. Nanite repair overheat lockouts (CannotActivateRepairingModule & CannotOverheatWhileRepairing)
    let sys = ctx.systems.get_or_spawn(30_000_142);
    sys.start_nanite_repair(ActiveNaniteRepair {
        char_id,
        ship_id,
        module_id: mod_plain,
        initial_damage_hp: 15.0,
        paste_total_needed: 8,
        paste_consumed_so_far: 0,
        duration_s: 2.0,
        elapsed_s: 0.0,
    })
    .await
    .unwrap();
    assert_eq!(
        check_nanite_repair_overheat_lockout(&sys, ship_id, mod_plain, "Activate", ModuleState::Online)
            .unwrap_err()
            .code,
        "CannotActivateRepairingModule"
    );
    assert_eq!(
        check_nanite_repair_overheat_lockout(&sys, ship_id, mod_raar, "Overload", ModuleState::Online)
            .unwrap_err()
            .code,
        "CannotOverheatWhileRepairing"
    );
    assert_eq!(
        check_nanite_repair_overheat_lockout(&sys, ship_id, mod_raar, "Activate", ModuleState::Overloaded)
            .unwrap_err()
            .code,
        "CannotOverheatWhileRepairing"
    );
    assert!(check_nanite_repair_overheat_lockout(&sys, ship_id, mod_raar, "Activate", ModuleState::Online).is_ok());

    // 3. compute_nanite_repair_params + cargo paste deduct & capped refund
    assert_eq!(compute_nanite_repair_params(None, 0.0, 40.0, 0, 0, 0).0, 0);
    let (p_tiny, r_tiny, d_tiny) = compute_nanite_repair_params(None, 0.001, 40.0, 0, 5, 0);
    assert_eq!(p_tiny, 1);
    assert!((r_tiny - 10.0).abs() < 1e-6);
    assert!((d_tiny - 1.0).abs() < 1e-6);
    let (p_scaled, r_scaled, d_scaled) = compute_nanite_repair_params(None, 40.0, 40.0, 0, 5, 5);
    assert_eq!(p_scaled, 15); // ceil(40 * 0.5 * 0.75) = 15
    assert!((r_scaled - 20.0).abs() < 1e-6);
    assert!((d_scaled - 2.0).abs() < 1e-6);

    ctx.db.with_mut_snapshot(|snap| {
        let ship_cap = snap.items[&ship_id].type_id;
        let hull_cap = ctx.sde.types[&ship_cap].capacity;
        let free0 = ship_free_cargo_m3_in_snap(snap, &ctx.sde, ship_id);
        assert!((free0 - hull_cap).abs() < 1e-6);
        // Zero/negative refund no-op
        assert_eq!(refund_paste_to_cargo_capped_in_snap(snap, &ctx.sde, char_id, ship_id, 0), 0);
        // Occupy cargo with tritanium (tid 34, 0.01 m3) leaving room for only ~5 paste units
        let paste_vol = ctx.sde.types.get(&28668).map(|t| t.volume.max(1e-6)).unwrap_or(0.01);
        let fill_qty = ((hull_cap - 5.0 * paste_vol) / 0.01).floor().max(0.0) as i32;
        snap.items.insert(1_000_950_090, InventoryItemRecord {
            item_id: 1_000_950_090,
            type_id: 34,
            owner_id: char_id,
            location_id: ship_id,
            flag: InventoryFlag::Cargo,
            quantity: fill_qty,
            singleton: false,
            version: 1,
            dynamic_attributes: HashMap::new(),
            source_mutaplasmid_id: None,
            insured_payout_isk: 0.0,
            is_blueprint_copy: false,
            blueprint_runs_remaining: None,
        });
        let free1 = ship_free_cargo_m3_in_snap(snap, &ctx.sde, ship_id);
        let expected_fit = ((free1 + 1e-9) / paste_vol).floor().max(0.0) as i32;
        assert_eq!(
            refund_paste_to_cargo_capped_in_snap(snap, &ctx.sde, char_id, ship_id, 100),
            expected_fit
        );
        // Partial deduct removes only needed units, full deduct removes stack
        assert_eq!(deduct_paste_from_cargo_in_snap(snap, char_id, ship_id, 2), 2);
        assert_eq!(
            refund_paste_to_cargo_capped_in_snap(snap, &ctx.sde, char_id, ship_id, 1),
            1
        );
    });

    // 4. evaluate_ship_thermodynamics + step_rack_heat_per_tick + apply_overheat_rack_and_damage_on_cycle
    let def_eval = evaluate_ship_thermodynamics(ship_id, &None, &None);
    assert!((def_eval.slot_ratio - 1.0).abs() < 1e-9);
    let missing_eval = evaluate_ship_thermodynamics(9_999_999, &Some(Arc::clone(&ctx.sde)), &Some(Arc::clone(&ctx.db)));
    assert!((missing_eval.slot_ratio - 1.0).abs() < 1e-9);

    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(char_id).or_default().insert(28164, TrainedSkillRecord {
            type_id: 28164,
            level: 5,
            skill_points: 256_000.0,
        });
        // Mark mod_cap_asb offline so slot_ratio counts only online hi/mid/low modules
        snap.module_online_states.insert(mod_cap_asb, false);
        // Seed legacy dynamic_attributes rack heat on ship for migration check
        let sh = snap.items.get_mut(&ship_id).unwrap();
        sh.dynamic_attributes.insert(RACK_HEAT_ATTR_IDS[0], 0.90);
        sh.dynamic_attributes.insert(RACK_HEAT_ATTR_IDS[1], 0.0);
        sh.dynamic_attributes.insert(RACK_HEAT_ATTR_IDS[2], 1.5);
    });
    let real_eval = evaluate_ship_thermodynamics(ship_id, &Some(Arc::clone(&ctx.sde)), &Some(Arc::clone(&ctx.db)));
    assert!(real_eval.slot_ratio > 0.0 && real_eval.slot_ratio <= 1.0);

    let mut rack_map = HashMap::new();
    let cycle_hi = ActiveModuleCycle {
        ship_id,
        module_id: mod_plain,
        module_type_id: 990_305,
        is_overloaded: true,
        rack_idx: 0,
        heat_damage_per_cycle: 20.0,
        ..Default::default()
    };
    let cycle_rack_clamp = ActiveModuleCycle {
        ship_id,
        module_id: mod_clamp_aar,
        module_type_id: 990_303,
        is_overloaded: true,
        rack_idx: 9, // clamps via .min(2) into low rack (idx 2)
        heat_damage_per_cycle: 0.0,
        ..Default::default()
    };
    // dt <= 0 / NaN are no-ops
    step_rack_heat_per_tick(&mut rack_map, &[cycle_hi.clone()], &Some(Arc::clone(&ctx.sde)), &Some(Arc::clone(&ctx.db)), 0.0);
    assert!(rack_map.is_empty());
    step_rack_heat_per_tick(&mut rack_map, &[cycle_hi.clone()], &Some(Arc::clone(&ctx.sde)), &Some(Arc::clone(&ctx.db)), f64::NAN);
    assert!(rack_map.is_empty());

    // Normal dt migrates legacy attrs ([0.90, 0.0 removed, 1.5->1.0]) and integrates heating
    step_rack_heat_per_tick(
        &mut rack_map,
        &[cycle_hi.clone(), cycle_rack_clamp],
        &Some(Arc::clone(&ctx.sde)),
        &Some(Arc::clone(&ctx.db)),
        1.0,
    );
    assert!(rack_map[&ship_id][0] >= 0.90);
    assert!((rack_map[&ship_id][1] - 0.0).abs() < 1e-9);
    assert!(ctx.db.with_snapshot(|s| !s.items[&ship_id].dynamic_attributes.contains_key(&RACK_HEAT_ATTR_IDS[0])));

    // Large cooling dt drops idle ship rack heat below 1e-6 threshold and clears transient attrs
    step_rack_heat_per_tick(&mut rack_map, &[], &Some(Arc::clone(&ctx.sde)), &Some(Arc::clone(&ctx.db)), 10_000.0);
    assert!(!rack_map.contains_key(&ship_id));
    assert!(ctx.db.with_snapshot(|s| !s.transient_item_attributes.contains_key(&ship_id)));

    // Set max rack heat on high rack (`0`) and trigger deterministic burnout on adjacent high slot (`mod_raar`)
    ctx.db.with_mut_snapshot(|snap| {
        snap.transient_item_attributes.entry(ship_id).or_default().insert(RACK_HEAT_ATTR_IDS[0], 1.0);
        // Set existing high damage on mod_raar (26.0 + eff_dmg 15.0 = 41.0 >= 40.0 -> burns out neighbor)
        snap.items.get_mut(&mod_raar).unwrap().dynamic_attributes.insert(attr_ids::DAMAGE, 26.0);
    });
    let rng = Arc::new(MechanicRng::seeded(TEST_MODE_DEFAULT_SEED));
    let mut burned_out = false;
    let mut burned_ship_modules = Vec::new();
    for _ in 0..12 {
        apply_overheat_rack_and_damage_on_cycle(
            &cycle_hi,
            &mut rack_map,
            &Some(Arc::clone(&ctx.sde)),
            &Some(Arc::clone(&ctx.db)),
            &Some(Arc::clone(&rng)),
            &mut burned_out,
            &mut burned_ship_modules,
        );
    }
    assert!(burned_out || !burned_ship_modules.is_empty(), "seeded heat propagation should burn at least one high-rack module");
    // Mid-rack and low-rack modules must NOT take high-rack overflow heat damage
    assert!(ctx.db.with_snapshot(|s| !s.items[&mod_clamp_aar].dynamic_attributes.contains_key(&attr_ids::DAMAGE)));
}
