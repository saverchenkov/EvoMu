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

    let rep_active = handle_active_overload_toggle(&ctx, &sys, char_id, ship_id, mod_id, fit, true).await;
    assert_eq!(rep_active.dict_get("overloaded").and_then(|v| v.as_bool()), Some(true));
}
