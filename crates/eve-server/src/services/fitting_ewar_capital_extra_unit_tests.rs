//! Deep branch-coverage and mutation-killing unit tests for `fitting_ewar_capital`.

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::inventory_fitting::fitting_ewar_capital::{
    commit_post_cycle_activation_effects, emit_committed_ewar_and_system_wide_notifications,
    prepare_ewar_and_capital_module, validate_passive_targeter_activation,
    DeferredPostCycleEffects, EwarCapitalPrep,
};
use eve_db::{InventoryFlag, InventoryItemRecord, TrainedSkillRecord, WorldRepository};
use eve_destiny::{Ball, CombatState, DVec3, MovementMode, SlimItemMeta, WarpPhase};
use eve_dogma::{attr_ids, DogmaItemState, FittedModule, ModuleState, ShipFittingEngine};
use eve_proto::BridgeRpcError;
use eve_sde::{DbuffCollectionInfo, InvType, SdeRegistry};
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

#[cfg(test)]
#[path = "fitting_ewar_and_burst_projectors_unit_tests.rs"]
mod fitting_ewar_and_burst_projectors_unit_tests;

pub(super) fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| {
        let mut sde = SdeRegistry::with_canonical_seed();
        sde.dbuff_collections.insert(9901, DbuffCollectionInfo {
            dbuff_id: 9901, name: "TestDbuff".into(), operation: "PostPercent".into(),
            aggregate_mode: "Maximum".into(), display_name_id: None, show_output_value_in_ui: None,
            item_modifiers: vec![37], location_modifiers: vec![], location_group_modifiers: vec![],
            location_required_skill_modifiers: vec![], target_attribute_ids: vec![],
        });
        Arc::new(sde)
    }))
}

pub(super) fn new_ctx() -> ServerContext {
    ServerContext::with_sde_and_repository(shared_sde(), Arc::new(WorldRepository::with_demo_seed()))
}

pub(super) fn synth_type(type_id: u32, group_id: u32, name: &str, effects: &[u16], attrs: &[(u16, f64)]) -> InvType {
    let mut t = shared_sde().types.get(&587).expect("seed type").clone();
    t.type_id = type_id; t.group_id = group_id; t.category_id = 7; t.name = name.into();
    t.effect_ids = effects.to_vec(); t.default_attributes.clear();
    for &(k, v) in attrs { t.default_attributes.insert(k, v); }
    t
}

pub(super) fn synth_module_and_state(
    module_id: i64, type_id: u32, flag_id: u16, charge_type_id: Option<u32>, attrs: &[(u16, f64)],
) -> (FittedModule, DogmaItemState) {
    let fm = FittedModule {
        item_id: module_id, type_id, flag_id, state: ModuleState::Online, charge_type_id,
        charge_quantity: if charge_type_id.is_some() { 1 } else { 0 },
        damage_hp: 0.0, max_hp: 40.0, dynamic_attribute_overrides: HashMap::new(),
    };
    let mut st = DogmaItemState {
        item_id: module_id, type_id, group_id: None, category_id: None,
        base_attributes: HashMap::new(), modifiers: Vec::new(), skill_levels: HashMap::new(),
    };
    for &(k, v) in attrs { st.base_attributes.insert(k, v); }
    (fm, st)
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn run_prep(
    ctx: &ServerContext, sys: &crate::system_actor::SystemHandle, sys_id: u32, char_id: i64,
    ship_id: i64, ship_group_id: u32, module_id: i64, fm: &FittedModule, mod_type: &InvType,
    mod_state: &DogmaItemState, fit: &ShipFittingEngine, target_id_opt: Option<i64>,
    is_overloaded: bool, is_mwd: bool, is_mjd: bool,
) -> Result<EwarCapitalPrep, BridgeRpcError> {
    let mut prep = prepare_ewar_and_capital_module(
        ctx, sys, sys_id, char_id, ship_id, ship_group_id, module_id, fm, mod_type, mod_state,
        fit, target_id_opt, is_overloaded, is_mwd, is_mjd, &HashMap::new(),
    )
    .await
    .expect("Some")?;
    let deferred = std::mem::take(&mut prep.deferred_post_cycle);
    commit_post_cycle_activation_effects(
        ctx, sys, char_id, ship_id, module_id, fm, mod_type, mod_state, target_id_opt, deferred,
    )
    .await?;
    Ok(prep)
}

pub(super) async fn spawn_test_ball(
    sys: &crate::system_actor::SystemHandle, id: i64, type_id: u32, pos: DVec3, mode: MovementMode, cap: f64,
) {
    let ball = Ball {
        id, type_id, position: pos, velocity: DVec3::ZERO, max_velocity: 300.0,
        inertia: 1.0, mass: 1_000_000.0, radius: 50.0, speed_fraction: 0.0, mode,
    };
    let mut cs = CombatState::default();
    cs.capacitor = cap;
    cs.max_capacitor = cap.max(1000.0);
    cs.signature_radius = 120.0;
    let meta = SlimItemMeta { group_id: 25, category_id: 6, owner_id: id, ..Default::default() };
    let _ = sys.spawn_ball_with_meta(ball, cs, meta).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn nullifier_adc_cehe_triage_block_and_zpme_branches() {
    let ctx = new_ctx();
    let sys_id = 30_000_142u32;
    let sys = ctx.systems.get_or_spawn(sys_id);
    let char_id = 97_100_001i64;
    let ship_id = 97_100_100i64;
    let mod_id = 97_100_200i64;
    let fit = ShipFittingEngine::from_sde(ship_id, 587, &ctx.sde);

    // 1. Interdiction Nullifier (group 4117) attribute fallbacks (canFitShipGroup & canFitShipType) and warp phase check
    let (fm, st) = synth_module_and_state(mod_id, 59600, 27, None, &[]);
    let nul_group_attr = synth_type(59600, 4117, "Nullifier GroupAttr", &[], &[(1298, 25.0)]);
    let ok_g = run_prep(
        &ctx, &sys, sys_id, char_id, ship_id, 25, mod_id, &fm, &nul_group_attr, &st, &fit, None, false, false, false,
    ).await.unwrap();
    assert!(ok_g.grants_interdiction_nullification);

    let nul_type_attr = synth_type(59600, 4117, "Nullifier TypeAttr", &[], &[(1302, 587.0)]);
    let ok_t = run_prep(
        &ctx, &sys, sys_id, char_id, ship_id, 9999, mod_id, &fm, &nul_type_attr, &st, &fit, None, false, false, false,
    ).await.unwrap();
    assert!(ok_t.grants_interdiction_nullification);

    // Aligning in warp is allowed; Cruising in warp is rejected with CannotActivateInWarp
    spawn_test_ball(
        &sys,
        ship_id,
        587,
        DVec3::ZERO,
        MovementMode::Warp {
            origin: DVec3::ZERO,
            destination: DVec3::new(1.0e9, 0.0, 0.0),
            phase: WarpPhase::Aligning,
            warp_speed_au_s: 3.0,
            total_distance: 1.0e9,
            elapsed_warp_time: 0.0,
        },
        1000.0,
    ).await;
    assert!(run_prep(
        &ctx, &sys, sys_id, char_id, ship_id, 831, mod_id, &fm, &nul_group_attr, &st, &fit, None, false, false, false,
    ).await.is_ok());

    spawn_test_ball(
        &sys,
        ship_id,
        587,
        DVec3::ZERO,
        MovementMode::Warp {
            origin: DVec3::ZERO,
            destination: DVec3::new(1.0e9, 0.0, 0.0),
            phase: WarpPhase::Cruising,
            warp_speed_au_s: 3.0,
            total_distance: 1.0e9,
            elapsed_warp_time: 5.0,
        },
        1000.0,
    ).await;
    assert_eq!(
        run_prep(&ctx, &sys, sys_id, char_id, ship_id, 831, mod_id, &fm, &nul_group_attr, &st, &fit, None, false, false, false)
            .await.err().unwrap().code,
        "CannotActivateInWarp"
    );
    spawn_test_ball(&sys, ship_id, 587, DVec3::ZERO, MovementMode::Stop, 1000.0).await;

    // 2. CEHE (40714..=40718 or effect 6484)
    let (cehe_fm, cehe_st) = synth_module_and_state(mod_id, 40714, 11, None, &[]);
    let cehe_type = synth_type(40714, 60, "Capital Emergency Hull Energizer I", &[6484], &[]);
    assert_eq!(
        run_prep(&ctx, &sys, sys_id, char_id, ship_id, 27, mod_id, &cehe_fm, &cehe_type, &cehe_st, &fit, None, false, false, false)
            .await.err().unwrap().code,
        "InvalidShipGroup"
    );
    assert!(run_prep(
        &ctx, &sys, sys_id, char_id, ship_id, 485, mod_id, &cehe_fm, &cehe_type, &cehe_st, &fit, None, false, false, false,
    ).await.is_ok());

    // 3. Active Triage blocks EWAR groups (201, 208, 291, 379, 52, 65, 1672)
    let mut triage_fit = fit.clone();
    triage_fit.modules.insert(
        999_001,
        FittedModule {
            item_id: 999_001,
            type_id: 27951,
            flag_id: 27,
            state: ModuleState::Active,
            charge_type_id: None,
            charge_quantity: 0,
            damage_hp: 0.0,
            max_hp: 40.0,
            dynamic_attribute_overrides: HashMap::new(),
        },
    );
    let ecm_type = synth_type(1952, 201, "Multispectral ECM I", &[], &[]);
    assert_eq!(
        run_prep(&ctx, &sys, sys_id, char_id, ship_id, 1538, mod_id, &fm, &ecm_type, &st, &triage_fit, Some(ship_id + 1), false, false, false)
            .await.err().unwrap().code,
        "EwarDisabledInTriage"
    );

    // 4. Zero-Point Mass Entangler (group 2008)
    let zpme_type = synth_type(49099, 2008, "Zero-Point Mass Entangler", &[], &[]);
    assert_eq!(
        run_prep(&ctx, &sys, sys_id, char_id, ship_id, 25, mod_id, &fm, &zpme_type, &st, &fit, None, false, false, false)
            .await.err().unwrap().code,
        "InvalidShipGroup"
    );
    // Default attributes (0.0 -> 0.20 mass, 0.05 vel)
    let zpme_def = run_prep(
        &ctx, &sys, sys_id, char_id, ship_id, 894, mod_id, &fm, &zpme_type, &st, &fit, None, false, false, false,
    ).await.unwrap();
    assert!(zpme_def.self_disable_mwd_mjd);
    assert_eq!(zpme_def.warp_core_stab_bonus, 2);
    assert!((zpme_def.self_mass_multiplier.unwrap() - 0.20).abs() < 1e-6);
    assert!((zpme_def.hic_velocity_multiplier.unwrap() - 0.05).abs() < 1e-6);

    // Explicit attributes (1131 = -70.0 -> 0.30, 2603 = -80.0 -> 0.20)
    let (_, zpme_st2) = synth_module_and_state(mod_id, 49099, 27, None, &[(1131, -70.0), (2603, -80.0)]);
    let zpme_custom = run_prep(
        &ctx, &sys, sys_id, char_id, ship_id, 4902, mod_id, &fm, &zpme_type, &zpme_st2, &fit, None, false, false, false,
    ).await.unwrap();
    assert!((zpme_custom.self_mass_multiplier.unwrap() - 0.30).abs() < 1e-6);
    assert!((zpme_custom.hic_velocity_multiplier.unwrap() - 0.20).abs() < 1e-6);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn siege_triage_bastion_indy_core_phenomena_mwd_and_mjd_branches() {
    let ctx = new_ctx();
    let sys_id = 30_000_142u32;
    let sys = ctx.systems.get_or_spawn(sys_id);
    let char_id = 97_100_002i64;
    let ship_id = 97_100_102i64;
    let victim_id = 97_100_103i64;
    let mod_id = 97_100_202i64;
    let fit = ShipFittingEngine::from_sde(ship_id, 587, &ctx.sde);

    spawn_test_ball(&sys, ship_id, 587, DVec3::ZERO, MovementMode::Stop, 1000.0).await;
    spawn_test_ball(&sys, victim_id, 587, DVec3::new(1000.0, 0.0, 0.0), MovementMode::Stop, 1000.0).await;
    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().cloned().unwrap();
        ch.character_id = char_id;
        ch.user_id = char_id as u64;
        ch.clone_grade = 2;
        snap.characters.insert(char_id, ch.clone());
        ch.character_id = char_id + 10;
        ch.user_id = (char_id + 10) as u64;
        snap.characters.insert(char_id + 10, ch);
        snap.items.insert(
            victim_id,
            InventoryItemRecord {
                item_id: victim_id,
                type_id: 587,
                owner_id: char_id + 10,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.skills.entry(char_id).or_default().insert(
            22043,
            TrainedSkillRecord { type_id: 22043, level: 4, skill_points: 100_000.0 },
        );
        snap.skills.entry(char_id).or_default().insert(
            27906,
            TrainedSkillRecord { type_id: 27906, level: 3, skill_points: 100_000.0 },
        );
    });

    // Siege hull check, missing fuel, insufficient fuel, and exact fuel consumption
    let (siege_fm, siege_st) = synth_module_and_state(mod_id, 20280, 27, None, &[]);
    let siege_type = synth_type(20280, 515, "Siege Module I", &[], &[(713, 16275.0), (714, 500.0)]);
    assert_eq!(
        run_prep(&ctx, &sys, sys_id, char_id, ship_id, 27, mod_id, &siege_fm, &siege_type, &siege_st, &fit, None, false, false, false)
            .await.err().unwrap().code,
        "InvalidShipGroup"
    );
    assert_eq!(
        run_prep(&ctx, &sys, sys_id, char_id, ship_id, 485, mod_id, &siege_fm, &siege_type, &siege_st, &fit, None, false, false, false)
            .await.err().unwrap().code,
        "InsufficientFuel"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            97_100_500,
            InventoryItemRecord {
                item_id: 97_100_500,
                type_id: 16275,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::SpecializedFuelBay,
                quantity: 10,
                singleton: false,
                ..Default::default()
            },
        );
    });
    assert_eq!(
        run_prep(&ctx, &sys, sys_id, char_id, ship_id, 485, mod_id, &siege_fm, &siege_type, &siege_st, &fit, None, false, false, false)
            .await.err().unwrap().code,
        "InsufficientFuel"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&97_100_500).unwrap().quantity = 150;
    });
    let siege_ok = run_prep(
        &ctx, &sys, sys_id, char_id, ship_id, 485, mod_id, &siege_fm, &siege_type, &siege_st, &fit, None, false, false, false,
    ).await.unwrap();
    assert!(siege_ok.immobilize_self && siege_ok.self_disallow_assistance && siege_ok.self_ewar_immune);
    assert_eq!(siege_ok.self_mass_multiplier, Some(10.0));
    assert!(ctx.db.with_snapshot(|s| !s.items.contains_key(&97_100_500)));

    // Triage hull check and partial fuel consumption + speed_factor_pct (-50% -> 0.50 vel mult)
    let (tri_fm, tri_st) = synth_module_and_state(mod_id, 27951, 27, None, &[(20, -50.0)]);
    let tri_type = synth_type(27951, 515, "Triage Module I", &[], &[(713, 16275.0), (714, 500.0)]);
    assert_eq!(
        run_prep(&ctx, &sys, sys_id, char_id, ship_id, 27, mod_id, &tri_fm, &tri_type, &tri_st, &fit, None, false, false, false)
            .await.err().unwrap().code,
        "InvalidShipGroup"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            97_100_501,
            InventoryItemRecord {
                item_id: 97_100_501,
                type_id: 16275,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::SpecializedFuelBay,
                quantity: 1000,
                singleton: false,
                ..Default::default()
            },
        );
    });
    let tri_ok = run_prep(
        &ctx, &sys, sys_id, char_id, ship_id, 1538, mod_id, &tri_fm, &tri_type, &tri_st, &fit, None, false, false, false,
    ).await.unwrap();
    assert!(!tri_ok.immobilize_self);
    assert!((tri_ok.hic_velocity_multiplier.unwrap() - 0.50).abs() < 1e-6);

    // Bastion hull check & crimewatch weapon timer
    let (bas_fm, bas_st) = synth_module_and_state(mod_id, 33400, 27, None, &[]);
    let bas_type = synth_type(33400, 515, "Bastion Module I", &[], &[]);
    assert_eq!(
        run_prep(&ctx, &sys, sys_id, char_id, ship_id, 27, mod_id, &bas_fm, &bas_type, &bas_st, &fit, None, false, false, false)
            .await.err().unwrap().code,
        "InvalidShipGroup"
    );
    let before_secs = ctx.now_unix_secs();
    let bas_ok = run_prep(
        &ctx, &sys, sys_id, char_id, ship_id, 900, mod_id, &bas_fm, &bas_type, &bas_st, &fit, None, false, false, false,
    ).await.unwrap();
    assert!(bas_ok.immobilize_self && !bas_ok.self_ewar_immune);
    let wep_until = ctx.crimewatch.write().unwrap().get_or_create(char_id).weapon_timer_until_unix;
    assert!(wep_until >= before_secs + 60);

    // Phenomena Generator (group 1815) hull check, isotope fuel consumption (never Strontium 16275), and dbuff application
    let (ph_fm, ph_st) = synth_module_and_state(mod_id, 43554, 27, None, &[(2468, 9901.0), (2469, 25.0)]);
    let ph_type = synth_type(43554, 1815, "Amarr Phenomena Generator", &[6753], &[(713, 16274.0), (714, 500.0)]);
    assert_eq!(
        run_prep(&ctx, &sys, sys_id, char_id, ship_id, 27, mod_id, &ph_fm, &ph_type, &ph_st, &fit, None, false, false, false)
            .await.err().unwrap().code,
        "InvalidShipGroup"
    );
    let stront_before_phen = ctx.db.with_snapshot(|s| s.items[&97_100_501].quantity);
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            97_100_502,
            InventoryItemRecord {
                item_id: 97_100_502,
                type_id: 16274,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::SpecializedFuelBay,
                quantity: 1000,
                singleton: false,
                ..Default::default()
            },
        );
    });
    let ph_ok = run_prep(
        &ctx, &sys, sys_id, char_id, ship_id, 30, mod_id, &ph_fm, &ph_type, &ph_st, &fit, None, false, false, false,
    ).await.unwrap();
    assert!(ph_ok.immobilize_self);
    assert_eq!(ctx.db.with_snapshot(|s| s.items[&97_100_501].quantity), stront_before_phen);
    assert_eq!(ctx.db.with_snapshot(|s| s.items[&97_100_502].quantity), 500);
    assert!(ctx.db.with_snapshot(|s| !s.items[&victim_id].dynamic_attributes.contains_key(&37)));
    assert!(
        crate::services::inventory_fitting::fitting_scanners_superweapons::get_aggregated_ship_dbuffs(
            &ctx,
            victim_id,
            ctx.now_unix_secs()
        )
        .contains_key(&9901)
    );

    // Propulsion (group 46) and MJD / MJFG
    let (prop_fm, prop_st) = synth_module_and_state(
        mod_id,
        597,
        19,
        None,
        &[(554, 500.0), (attr_ids::MASS_ADDITION, 500_000.0)],
    );
    let prop_type = synth_type(597, 46, "5MN Microwarpdrive I", &[], &[]);
    let prop_ok = run_prep(
        &ctx, &sys, sys_id, char_id, ship_id, 25, mod_id, &prop_fm, &prop_type, &prop_st, &fit, None, false, true, false,
    ).await.unwrap();
    assert!((prop_ok.hic_sig_multiplier.unwrap() - 6.0).abs() < 1e-6);
    assert_eq!(prop_ok.hic_mass_addition, Some(500_000.0));

    let (mjd_fm, mjd_st) = synth_module_and_state(
        mod_id,
        37480,
        19,
        None,
        &[(2066, 100_000.0), (2067, 6_000.0), (2832, 24.0), (5686, 4.0), (973, 100.0)],
    );
    let mjd_type = synth_type(37480, 1533, "Micro Jump Field Generator", &[], &[(1298, 1534.0)]);
    assert_eq!(
        run_prep(&ctx, &sys, sys_id, char_id, ship_id, 25, mod_id, &mjd_fm, &mjd_type, &mjd_st, &fit, None, false, false, true)
            .await.err().unwrap().code,
        "InvalidShipGroup"
    );
    let mjd_ok = run_prep(
        &ctx, &sys, sys_id, char_id, ship_id, 1534, mod_id, &mjd_fm, &mjd_type, &mjd_st, &fit, None, false, false, true,
    ).await.unwrap();
    assert_eq!(mjd_ok.mjd_jump_distance_m, Some(100_000.0));
    assert_eq!(mjd_ok.mjfg_field_params, Some((6_000.0, 25, 4)));
    assert!((mjd_ok.hic_sig_multiplier.unwrap() - 2.0).abs() < 1e-6);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn passive_targeter_remote_rep_projected_assist_and_deferred_charges_branches() {
    let ctx = new_ctx();
    let sys_id = 30_000_142_u32;
    let sys = ctx.systems.get_or_spawn(sys_id);
    let char_id = 97_100_010_i64;
    let ship_id = 97_100_110_i64;
    let target_id = 97_100_111_i64;
    let mod_id = 97_100_210_i64;
    let fit = ShipFittingEngine::from_sde(ship_id, 587, &ctx.sde);
    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.get(&90_000_001).cloned().unwrap();
        ch.character_id = char_id;
        ch.solar_system_id = sys_id;
        ch.station_id = None;
        ch.active_ship_id = ship_id;
        snap.characters.insert(char_id, ch);
    });

    // Interdiction Nullifier wrong ship group -> InvalidShipGroup (`L118-123`)
    let (nul_fm, nul_st) = synth_module_and_state(mod_id, 59600, 27, None, &[]);
    let nul_type = synth_type(59600, 4117, "Nullifier", &[], &[(1298, 25.0)]);
    assert_eq!(
        run_prep(&ctx, &sys, sys_id, char_id, ship_id, 27, mod_id, &nul_fm, &nul_type, &nul_st, &fit, None, false, false, false)
            .await
            .err()
            .unwrap()
            .code,
        "InvalidShipGroup"
    );

    // Passive targeter validation branches (`L852-920`) before spawning balls
    assert_eq!(
        validate_passive_targeter_activation(&ctx, &sys, char_id, ship_id, target_id, &fit)
            .await
            .unwrap_err()
            .code,
        "TargetingFailed"
    );
    spawn_test_ball(&sys, ship_id, 587, DVec3::ZERO, MovementMode::Stop, 1000.0).await;
    assert_eq!(
        validate_passive_targeter_activation(&ctx, &sys, char_id, ship_id, target_id, &fit)
            .await
            .unwrap_err()
            .code,
        "TargetingFailed"
    );
    spawn_test_ball(&sys, target_id, 587, DVec3::new(1000.0, 0.0, 0.0), MovementMode::Stop, 1000.0).await;

    // Self jammed (`ShipJammed`), self cloaked (`TargetingFailed`), target cloaked (`TargetingFailed`), target tethered (`TargetInvulnerable`)
    {
        let mut bp = sys.ballpark.write().unwrap();
        bp.combat_states.get_mut(&ship_id).unwrap().ecm_jammed_remaining_s = 5.0;
    }
    assert_eq!(
        validate_passive_targeter_activation(&ctx, &sys, char_id, ship_id, target_id, &fit)
            .await
            .unwrap_err()
            .code,
        "ShipJammed"
    );
    {
        let mut bp = sys.ballpark.write().unwrap();
        let scs = bp.combat_states.get_mut(&ship_id).unwrap();
        scs.ecm_jammed_remaining_s = 0.0;
        scs.is_cloaked = true;
    }
    assert_eq!(
        validate_passive_targeter_activation(&ctx, &sys, char_id, ship_id, target_id, &fit)
            .await
            .unwrap_err()
            .code,
        "TargetingFailed"
    );
    {
        let mut bp = sys.ballpark.write().unwrap();
        bp.combat_states.get_mut(&ship_id).unwrap().is_cloaked = false;
        bp.combat_states.get_mut(&target_id).unwrap().is_cloaked = true;
    }
    assert_eq!(
        validate_passive_targeter_activation(&ctx, &sys, char_id, ship_id, target_id, &fit)
            .await
            .unwrap_err()
            .code,
        "TargetingFailed"
    );
    {
        let mut bp = sys.ballpark.write().unwrap();
        let tcs = bp.combat_states.get_mut(&target_id).unwrap();
        tcs.is_cloaked = false;
        tcs.is_tethered = true;
    }
    assert_eq!(
        validate_passive_targeter_activation(&ctx, &sys, char_id, ship_id, target_id, &fit)
            .await
            .unwrap_err()
            .code,
        "TargetInvulnerable"
    );
    // Out of range & TooManyLockedTargets
    {
        let mut bp = sys.ballpark.write().unwrap();
        bp.combat_states.get_mut(&target_id).unwrap().is_tethered = false;
        bp.balls.get_mut(&target_id).unwrap().position = DVec3::new(9_000_000.0, 0.0, 0.0);
    }
    assert_eq!(
        validate_passive_targeter_activation(&ctx, &sys, char_id, ship_id, target_id, &fit)
            .await
            .unwrap_err()
            .code,
        "TargetOutOfRange"
    );
    {
        let mut bp = sys.ballpark.write().unwrap();
        bp.balls.get_mut(&target_id).unwrap().position = DVec3::new(1000.0, 0.0, 0.0);
    }
    for dummy in 1..=10_i64 {
        let _ = sys
            .add_target_lock(
                ship_id,
                eve_destiny::TargetLock {
                    target_id: target_id + dummy,
                    lock_time_ms: 1000.0,
                    remaining_lock_ms: 0.0,
                    locked: true,
                    max_range_m: None,
                },
            )
            .await;
    }
    assert_eq!(
        validate_passive_targeter_activation(&ctx, &sys, char_id, ship_id, target_id, &fit)
            .await
            .unwrap_err()
            .code,
        "TooManyLockedTargets"
    );

    // Projected remote assist group 290/209: TargetRequired, OutOfRange, success (`L176-195`, `L1026-1035`)
    let (pa_fm, pa_st) = synth_module_and_state(mod_id, 99801, 19, None, &[(54, 15_000.0), (2044, 5_000.0)]);
    let pa_type = synth_type(99801, 290, "Remote Tracking Computer I", &[], &[]);
    assert_eq!(
        run_prep(&ctx, &sys, sys_id, char_id, ship_id, 25, mod_id, &pa_fm, &pa_type, &pa_st, &fit, None, false, false, false)
            .await
            .err()
            .unwrap()
            .code,
        "TargetRequired"
    );
    {
        let mut bp = sys.ballpark.write().unwrap();
        bp.balls.get_mut(&target_id).unwrap().position = DVec3::new(50_000.0, 0.0, 0.0);
    }
    assert_eq!(
        run_prep(&ctx, &sys, sys_id, char_id, ship_id, 25, mod_id, &pa_fm, &pa_type, &pa_st, &fit, Some(target_id), false, false, false)
            .await
            .err()
            .unwrap()
            .code,
        "OutOfRange"
    );
    {
        let mut bp = sys.ballpark.write().unwrap();
        bp.balls.get_mut(&target_id).unwrap().position = DVec3::new(1_000.0, 0.0, 0.0);
    }
    assert!(
        run_prep(&ctx, &sys, sys_id, char_id, ship_id, 25, mod_id, &pa_fm, &pa_type, &pa_st, &fit, Some(target_id), false, false, false)
            .await
            .is_ok()
    );

    // Remote assistance (group 325/585) TargetRequired, CannotActivateOnSelf, TargetOutOfRange, normal armor repair (`L680-754`)
    let (rr_fm, rr_st) = synth_module_and_state(mod_id, 3530, 27, None, &[(54, 5_000.0), (84, 100.0)]);
    let rr_type = synth_type(3530, 325, "Small Remote Armor Repairer I", &[], &[]);
    assert_eq!(
        run_prep(&ctx, &sys, sys_id, char_id, ship_id, 25, mod_id, &rr_fm, &rr_type, &rr_st, &fit, None, false, false, false)
            .await
            .err()
            .unwrap()
            .code,
        "TargetRequired"
    );
    assert_eq!(
        run_prep(&ctx, &sys, sys_id, char_id, ship_id, 25, mod_id, &rr_fm, &rr_type, &rr_st, &fit, Some(ship_id), false, false, false)
            .await
            .err()
            .unwrap()
            .code,
        "CannotActivateOnSelf"
    );
    {
        let mut bp = sys.ballpark.write().unwrap();
        bp.balls.get_mut(&target_id).unwrap().position = DVec3::new(10_000.0, 0.0, 0.0);
    }
    assert_eq!(
        run_prep(&ctx, &sys, sys_id, char_id, ship_id, 25, mod_id, &rr_fm, &rr_type, &rr_st, &fit, Some(target_id), false, false, false)
            .await
            .err()
            .unwrap()
            .code,
        "TargetOutOfRange"
    );
    {
        let mut bp = sys.ballpark.write().unwrap();
        bp.balls.get_mut(&target_id).unwrap().position = DVec3::new(1_000.0, 0.0, 0.0);
    }
    let rr_ok = run_prep(&ctx, &sys, sys_id, char_id, ship_id, 25, mod_id, &rr_fm, &rr_type, &rr_st, &fit, Some(target_id), false, false, false)
        .await
        .unwrap();
    assert!((rr_ok.remote_armor_amount - 100.0).abs() < 1e-6);

    // commit_post_cycle_activation_effects consuming cap booster & turret charge (`L989-1015`) + OnSystemWideEffectStart char fallback (`L1186-1192`)
    let cap_cid = 97_100_901_i64;
    let ammo_cid = 97_100_902_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(cap_cid, InventoryItemRecord {
            item_id: cap_cid, type_id: 263, owner_id: char_id, location_id: ship_id,
            flag: InventoryFlag::Cargo, quantity: 1, ..Default::default()
        });
        snap.items.insert(ammo_cid, InventoryItemRecord {
            item_id: ammo_cid, type_id: 215, owner_id: char_id, location_id: ship_id,
            flag: InventoryFlag::HiSlot0, quantity: 1, ..Default::default()
        });
        snap.module_active_charges.insert(mod_id, 215);
    });
    let mut def = DeferredPostCycleEffects::default();
    def.cap_booster_charge_id = Some(cap_cid);
    def.turret_charge_consumptions.push((mod_id, ammo_cid));
    commit_post_cycle_activation_effects(&ctx, &sys, char_id, ship_id, mod_id, &rr_fm, &rr_type, &rr_st, None, def)
        .await
        .unwrap();
    ctx.db.with_snapshot(|s| {
        assert!(!s.items.contains_key(&cap_cid));
        assert!(!s.items.contains_key(&ammo_cid));
        assert!(!s.module_active_charges.contains_key(&mod_id));
    });
    let ph_type = synth_type(43554, 1815, "Amarr Phenomena Generator", &[6753], &[]);
    emit_committed_ewar_and_system_wide_notifications(&ctx, sys_id, char_id, ship_id, mod_id, 43554, &ph_type, None, false, None, None);
    assert!(
        !ctx.drain_notifications_for_char(char_id)
            .iter()
            .any(|n| n.event == "OnSystemWideEffectStart"),
        "Phenomena Generator must never emit OnSystemWideEffectStart"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn target_painter_threshold_group_guard_and_fuel_boundary_kill_mutants() {
    let ctx = new_ctx();
    let sys_id = 30_000_142u32;
    let sys = ctx.systems.get_or_spawn(sys_id);
    let char_id = 90_880_001i64;
    let ship_id = 97_880_001i64;
    let target_id = 97_880_002i64;
    let mod_id = 97_880_010i64;

    spawn_test_ball(&sys, ship_id, 587, DVec3::ZERO, MovementMode::Stop, 5000.0).await;
    spawn_test_ball(&sys, target_id, 587, DVec3::new(1_000.0, 0.0, 0.0), MovementMode::Stop, 5000.0).await;
    let fit = ShipFittingEngine::from_sde(ship_id, 587, &ctx.sde);

    // 1. Target Painter (group 379) with bonus=25% -> tp_sig_multiplier == Some(1.25)
    let tp_type = synth_type(99_881, 379, "Target Painter I", &[], &[]);
    let (tp_fm, tp_st_25) = synth_module_and_state(mod_id, 99_881, 19, None, &[(54, 50_000.0), (2044, 20_000.0), (554, 25.0)]);
    let _ = run_prep(&ctx, &sys, sys_id, char_id, ship_id, 25, mod_id, &tp_fm, &tp_type, &tp_st_25, &fit, Some(target_id), false, false, false)
        .await
        .unwrap();
    let tp_25 = sys.get_combat_state(target_id).await.unwrap().unwrap().tp_sig_multiplier;
    assert!(tp_25.is_some_and(|v| (v - 1.25).abs() < 1e-6), "expected 1.25, got {tp_25:?}");

    // Non-TP EWAR (web group 65) while projected TP exists: clears manual reset first, MUST NOT set tp_sig_multiplier
    sys.try_recompute_combat_attributes(target_id, |cs| { cs.tp_sig_multiplier = None; });
    let web_type = synth_type(99_882, 65, "Stasis Webifier I", &[], &[]);
    let (web_fm, web_st) = synth_module_and_state(mod_id + 1, 99_882, 20, None, &[(54, 10_000.0), (20, -50.0)]);
    let _ = run_prep(&ctx, &sys, sys_id, char_id, ship_id, 25, mod_id + 1, &web_fm, &web_type, &web_st, &fit, Some(target_id), false, false, false)
        .await
        .unwrap();
    assert_eq!(
        sys.get_combat_state(target_id).await.unwrap().unwrap().tp_sig_multiplier,
        None,
        "group_id != 379 must not recompute tp_sig_multiplier"
    );

    // Clear projected EWAR and test tp_mult == 1.0 (bonus=0.0 -> None, kills 1.0 - 1e-6 and 1.0 * 1e-6)
    ctx.projected_ewar.write().unwrap().remove(&target_id);
    let (_, tp_st_0) = synth_module_and_state(mod_id, 99_881, 19, None, &[(54, 50_000.0), (2044, 20_000.0), (554, 0.0)]);
    let _ = run_prep(&ctx, &sys, sys_id, char_id, ship_id, 25, mod_id, &tp_fm, &tp_type, &tp_st_0, &fit, Some(target_id), false, false, false)
        .await
        .unwrap();
    assert_eq!(sys.get_combat_state(target_id).await.unwrap().unwrap().tp_sig_multiplier, None);

    // Exact boundary tp_mult == 1.0 + 1e-6 (bonus = 1e-4%) -> strictly `>` must yield None (kills `>` -> `>=`)
    ctx.projected_ewar.write().unwrap().remove(&target_id);
    let (_, tp_st_edge) = synth_module_and_state(mod_id, 99_881, 19, None, &[(54, 50_000.0), (2044, 20_000.0), (554, 1e-4)]);
    let _ = run_prep(&ctx, &sys, sys_id, char_id, ship_id, 25, mod_id, &tp_fm, &tp_type, &tp_st_edge, &fit, Some(target_id), false, false, false)
        .await
        .unwrap();
    assert_eq!(sys.get_combat_state(target_id).await.unwrap().unwrap().tp_sig_multiplier, None);

    // 2. commit_post_cycle_activation_effects fuel deduction exact boundary: avail==4 untouched, avail==5 consumed
    let fuel_tid = 17888_u32;
    let fuel_id = 97_880_500_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            fuel_id,
            InventoryItemRecord {
                item_id: fuel_id,
                type_id: fuel_tid,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 4,
                ..Default::default()
            },
        );
    });
    let mut def_short = DeferredPostCycleEffects::default();
    def_short.fuel_to_deduct = Some((fuel_tid, 5));
    commit_post_cycle_activation_effects(&ctx, &sys, char_id, ship_id, mod_id, &tp_fm, &tp_type, &tp_st_25, None, def_short)
        .await
        .unwrap();
    assert_eq!(ctx.db.with_snapshot(|s| s.items[&fuel_id].quantity), 4);

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&fuel_id).unwrap().quantity = 5;
    });
    let mut def_exact = DeferredPostCycleEffects::default();
    def_exact.fuel_to_deduct = Some((fuel_tid, 5));
    commit_post_cycle_activation_effects(&ctx, &sys, char_id, ship_id, mod_id, &tp_fm, &tp_type, &tp_st_25, None, def_exact)
        .await
        .unwrap();
    assert!(ctx.db.with_snapshot(|s| !s.items.contains_key(&fuel_id)));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn phenomena_fuel_ignores_strontium_prioritizes_fuel_bay_sums_stacks_and_notifies_gh1037() {
    use eve_proto::PyRep;

    let ctx = new_ctx();
    let sys_id = 30_000_142_u32;
    let sys = ctx.systems.get_or_spawn(sys_id);
    let char_id = 90_000_001_i64;
    let ship_id = 97_990_001_i64;
    let mod_id = 97_990_010_i64;
    let fit = ShipFittingEngine::from_sde(ship_id, 11567, &ctx.sde);
    spawn_test_ball(&sys, ship_id, 11567, DVec3::ZERO, MovementMode::Stop, 50_000.0).await;
    let _ = ctx.db.acquire_online_lock(1, 1, Some(char_id), false, ctx.now_unix());
    let _ = ctx.drain_notifications_for_char(char_id);

    let (ph_fm, ph_st) = synth_module_and_state(mod_id, 43658, 27, None, &[]);
    let ph_type = synth_type(
        43658,
        1815,
        "Amarr Phenomena Generator",
        &[6753],
        &[(713, 16274.0), (714, 40_000.0)],
    );

    // 1. An earlier Cargo stack of Strontium Clathrates (16275) must NOT match or shadow Helium Isotopes (16274).
    let stront_id = 1_897_129_275_i64;
    let fuel_bay_id = 1_897_129_283_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            stront_id,
            InventoryItemRecord {
                item_id: stront_id,
                type_id: 16275,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 5_000,
                singleton: false,
                ..Default::default()
            },
        );
    });
    let missing_err = run_prep(
        &ctx, &sys, sys_id, char_id, ship_id, 30, mod_id, &ph_fm, &ph_type, &ph_st, &fit, None, false, false, false,
    )
    .await
    .err()
    .expect("Strontium 16275 must not satisfy Phenomena Generator needing 16274");
    assert_eq!(missing_err.code, "InsufficientFuel");
    assert!(missing_err.message.contains("Missing required fuel"));

    // 2. Add 80,000 Helium Isotopes (16274) in SpecializedFuelBay (flag 133) with a higher item_id:
    //    must ignore the earlier 5,000 Strontium stack, deduct 40,000 Helium, and emit OnItemChange.
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            fuel_bay_id,
            InventoryItemRecord {
                item_id: fuel_bay_id,
                type_id: 16274,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::SpecializedFuelBay,
                quantity: 80_000,
                singleton: false,
                ..Default::default()
            },
        );
    });
    let _ = ctx.drain_notifications_for_char(char_id);
    run_prep(
        &ctx, &sys, sys_id, char_id, ship_id, 30, mod_id, &ph_fm, &ph_type, &ph_st, &fit, None, false, false, false,
    )
    .await
    .expect("80,000 Helium in ShipFuelBay must satisfy Phenomena Generator despite earlier 5,000 Strontium");

    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items[&stront_id].quantity, 5_000, "Strontium must stay untouched");
        assert_eq!(snap.items[&fuel_bay_id].quantity, 40_000, "Helium in FuelBay must drop by 40,000");
    });
    let notifs_1 = ctx.drain_notifications_for_char(char_id);
    assert!(
        notifs_1.iter().any(|n| {
            n.event == "OnItemChange"
                && n.args[0].dbrow_get("itemID") == Some(&PyRep::Integer(fuel_bay_id))
                && n.args[0].dbrow_get("quantity") == Some(&PyRep::Integer(40_000))
        }),
        "OnItemChange must update the Helium FuelBay stack to 40,000"
    );

    // 3. Multi-stack summing & FuelBay-before-Cargo priority (DroneBay ignored):
    //    FuelBay (higher item_id) = 25,000 + Cargo (lower item_id) = 20,000 -> total 45,000 >= 40,000.
    //    Must consume all 25,000 from FuelBay first (deleting it) and 15,000 from Cargo (leaving 5,000),
    //    leaving DroneBay untouched, and emitting OnItemChange for both deleted and updated stacks.
    let cargo_iso_id = 1_897_129_276_i64;
    let drone_bay_iso_id = 1_897_129_277_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&fuel_bay_id).unwrap().quantity = 25_000;
        snap.items.insert(
            cargo_iso_id,
            InventoryItemRecord {
                item_id: cargo_iso_id,
                type_id: 16274,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 20_000,
                singleton: false,
                ..Default::default()
            },
        );
        snap.items.insert(
            drone_bay_iso_id,
            InventoryItemRecord {
                item_id: drone_bay_iso_id,
                type_id: 16274,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::DroneBay,
                quantity: 50_000,
                singleton: false,
                ..Default::default()
            },
        );
    });
    let _ = ctx.drain_notifications_for_char(char_id);
    run_prep(
        &ctx, &sys, sys_id, char_id, ship_id, 30, mod_id, &ph_fm, &ph_type, &ph_st, &fit, None, false, false, false,
    )
    .await
    .expect("25,000 FuelBay + 20,000 Cargo must sum to 45,000 >= 40,000");

    ctx.db.with_snapshot(|snap| {
        assert!(!snap.items.contains_key(&fuel_bay_id), "FuelBay stack must be consumed first and deleted");
        assert_eq!(snap.items[&cargo_iso_id].quantity, 5_000, "Remaining 15,000 must come from Cargo");
        assert_eq!(snap.items[&drone_bay_iso_id].quantity, 50_000, "DroneBay must be ignored");
    });
    let notifs_2 = ctx.drain_notifications_for_char(char_id);
    assert!(
        notifs_2.iter().any(|n| {
            n.event == "OnItemChange"
                && n.args[0].dbrow_get("itemID") == Some(&PyRep::Integer(fuel_bay_id))
                && n.args[0].dbrow_get("locationID") == Some(&PyRep::Integer(6))
                && n.args[0].dbrow_get("quantity") == Some(&PyRep::Integer(0))
        }),
        "Deleted FuelBay stack must emit OnItemChange tombstone"
    );
    assert!(
        notifs_2.iter().any(|n| {
            n.event == "OnItemChange"
                && n.args[0].dbrow_get("itemID") == Some(&PyRep::Integer(cargo_iso_id))
                && n.args[0].dbrow_get("quantity") == Some(&PyRep::Integer(5_000))
        }),
        "Partially consumed Cargo stack must emit OnItemChange quantity update"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn weapon_autoload_emits_tombstones_subloc_conserves_bank_and_rejects_tombstoned_loadammo_gh1124() {
    use eve_proto::PyRep;

    let mut ctx = new_ctx();
    let sys_id = 30_000_142_u32;
    let char_id = 90_000_001_i64;
    let ship_id = 97_990_201_i64;
    let turret_single = 97_990_210_i64;
    let cargo_stack_1 = 97_990_211_i64;
    let turret_b1 = 97_990_220_i64;
    let turret_b2 = 97_990_221_i64;
    let turret_b3 = 97_990_222_i64;
    let locked_cargo_ammo = 97_990_223_i64;
    let singleton_cargo_ammo = 97_990_224_i64;
    let cargo_stack_2 = 97_990_225_i64;

    let mut ac_type = synth_type(55_901, 55, "200mm AutoCannon I", &[10], &[(604, 83.0), (128, 1.0)]);
    ac_type.capacity = 5.0;
    let mut ammo_type = synth_type(83_901, 83, "Titanium Sabot S", &[], &[(128, 1.0), (114, 10.0)]);
    ammo_type.category_id = 8;
    ammo_type.volume = 0.0025;
    {
        let sde_mut = Arc::make_mut(&mut ctx.sde);
        sde_mut.types.insert(55_901, ac_type.clone());
        sde_mut.types.insert(83_901, ammo_type);
    }

    let sys = ctx.systems.get_or_spawn(sys_id);
    spawn_test_ball(&sys, ship_id, 587, DVec3::ZERO, MovementMode::Stop, 500.0).await;
    let _ = ctx.db.acquire_online_lock(1, 1, Some(char_id), false, ctx.now_unix());

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.active_ship_id = ship_id;
            ch.solar_system_id = sys_id;
            ch.station_id = None;
            ch.structure_id = None;
        }
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 587,
                owner_id: char_id,
                location_id: sys_id as i64,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            turret_single,
            InventoryItemRecord {
                item_id: turret_single,
                type_id: 55_901,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            cargo_stack_1,
            InventoryItemRecord {
                item_id: cargo_stack_1,
                type_id: 83_901,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 2,
                singleton: false,
                ..Default::default()
            },
        );
    });

    let mut fit = ShipFittingEngine::from_sde(ship_id, 587, &ctx.sde);
    let _ = fit.fit_module_with_state(turret_single, 55_901, 27, true, &ctx.sde);
    let (fm_single, st_single) = synth_module_and_state(turret_single, 55_901, 27, None, &[]);
    let _ = ctx.drain_notifications_for_char(char_id);

    // 1. Silent auto-load on Activate empties Cargo stack -> OnItemChange tombstone + slot prime + attr 805 + OnChargeLoadedToModule
    run_prep(
        &ctx, &sys, sys_id, char_id, ship_id, 25, turret_single, &fm_single, &ac_type, &st_single, &fit, None, false, false, false,
    )
    .await
    .expect("autoload succeeds");

    let slot0_charge_id = ctx.db.with_snapshot(|snap| {
        assert!(!snap.items.contains_key(&cargo_stack_1), "consumed cargo stack must be removed");
        snap.items
            .values()
            .find(|i| i.location_id == ship_id && i.flag == InventoryFlag::HiSlot0 && i.item_id != turret_single)
            .map(|i| (i.item_id, i.quantity, i.type_id))
            .expect("slot charge created")
    });
    assert_eq!((slot0_charge_id.1, slot0_charge_id.2), (2, 83_901));

    let notifs = ctx.drain_notifications_for_char(char_id);
    assert!(
        notifs.iter().any(|n| {
            n.event == "OnItemChange"
                && n.args[0].dbrow_get("itemID") == Some(&PyRep::Integer(cargo_stack_1))
                && n.args[0].dbrow_get("locationID") == Some(&PyRep::Integer(6))
                && n.args[0].dbrow_get("flagID") == Some(&PyRep::Integer(0))
                && n.args[0].dbrow_get("quantity") == Some(&PyRep::Integer(0))
        }),
        "depleted cargo stack must emit OnItemChange tombstone (location=6, flag=0, qty=0)"
    );
    assert!(
        notifs.iter().any(|n| {
            n.event == "OnItemChange"
                && n.args[0].dbrow_get("itemID") == Some(&PyRep::Integer(slot0_charge_id.0))
                && n.args[0].dbrow_get("flagID") == Some(&PyRep::Integer(27))
                && n.args[0].dbrow_get("quantity") == Some(&PyRep::Integer(2))
        }),
        "loaded slot charge must emit OnItemChange in HiSlot0"
    );
    assert!(notifs.iter().any(|n| n.event == "OnGodmaPrimeItem"), "autoload must prime sublocation");
    assert!(
        notifs.iter().any(|n| n.event == "OnModuleAttributeChanges"),
        "autoload must emit OnModuleAttributeChanges (805)"
    );
    assert!(notifs.iter().any(|n| n.event == "OnChargeLoadedToModule"));

    // Calling LoadAmmo directly with the now-tombstoned cargo_stack_1 must still fail with ItemNotFound
    let mut sess = eve_machonet::SessionState::new(1124);
    sess.user_id = Some(1);
    sess.character_id = Some(char_id);
    sess.ship_id = Some(ship_id);
    sess.solar_system_id = Some(sys_id);
    let load_err = crate::services::notify_emitters_seasons_tactical::handle_load_ammo(
        &ctx,
        &mut sess,
        &[
            PyRep::Integer(ship_id),
            PyRep::Integer(turret_single),
            PyRep::List(vec![PyRep::Integer(cargo_stack_1)]),
            PyRep::Integer(ship_id),
        ],
    )
    .expect_err("tombstoned cargo_stack_1 must return ItemNotFound, never fake Ok");
    assert_eq!(load_err.code, "ItemNotFound");

    // 2. Moving an item from Cargo -> HiSlot via emit_ship_charge_item_diffs also emits OnGodmaPrimeItem
    let pre_move = InventoryItemRecord {
        item_id: 97_990_299,
        type_id: 83_901,
        owner_id: char_id,
        location_id: ship_id,
        flag: InventoryFlag::Cargo,
        quantity: 3,
        ..Default::default()
    };
    let mut post_move = pre_move.clone();
    post_move.flag = InventoryFlag::HiSlot1;
    let _ = ctx.drain_notifications_for_char(char_id);
    crate::services::inventory_fitting::fitting_module_cycles::emit_ship_charge_item_diffs(
        &ctx,
        char_id,
        ship_id,
        &[(Some(pre_move), Some(post_move))],
    );
    let move_notifs = ctx.drain_notifications_for_char(char_id);
    assert!(
        move_notifs.iter().any(|n| n.event == "OnGodmaPrimeItem"),
        "cargo-to-fitted-slot transition via emit_ship_charge_item_diffs must emit OnGodmaPrimeItem"
    );

    // 3. Grouped 3-gun bank with avail_qty=2 (< bank.len()) must conserve total rounds (sum == 2)
    //    and ignore lower-ID Locked or singleton cargo stacks.
    ctx.db.with_mut_snapshot(|snap| {
        for (tid, flag) in [
            (turret_b1, InventoryFlag::HiSlot1),
            (turret_b2, InventoryFlag::HiSlot2),
            (turret_b3, InventoryFlag::HiSlot3),
        ] {
            snap.items.insert(
                tid,
                InventoryItemRecord {
                    item_id: tid,
                    type_id: 55_901,
                    owner_id: char_id,
                    location_id: ship_id,
                    flag,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
        }
        snap.weapon_banks
            .entry(ship_id)
            .or_default()
            .insert(turret_b1, vec![turret_b1, turret_b2, turret_b3]);
        snap.items.insert(
            locked_cargo_ammo,
            InventoryItemRecord {
                item_id: locked_cargo_ammo,
                type_id: 83_901,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 10,
                singleton: false,
                ..Default::default()
            },
        );
        snap.kv_mut(eve_db::Ns::AlscLockedItem)
            .set_with_unix(locked_cargo_ammo, "", &true, 1)
            .unwrap();
        snap.items.insert(
            singleton_cargo_ammo,
            InventoryItemRecord {
                item_id: singleton_cargo_ammo,
                type_id: 83_901,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            cargo_stack_2,
            InventoryItemRecord {
                item_id: cargo_stack_2,
                type_id: 83_901,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 2,
                singleton: false,
                ..Default::default()
            },
        );
    });
    let (fm_b1, st_b1) = synth_module_and_state(turret_b1, 55_901, 28, None, &[]);
    let _ = ctx.drain_notifications_for_char(char_id);
    run_prep(
        &ctx, &sys, sys_id, char_id, ship_id, 25, turret_b1, &fm_b1, &ac_type, &st_b1, &fit, None, false, false, false,
    )
    .await
    .expect("grouped bank autoload succeeds");

    ctx.db.with_snapshot(|snap| {
        assert!(!snap.items.contains_key(&cargo_stack_2), "cargo_stack_2 must be fully consumed");
        assert_eq!(snap.items[&locked_cargo_ammo].quantity, 10, "contract-locked stack untouched");
        assert!(snap.items.contains_key(&singleton_cargo_ammo), "singleton stack untouched");
        let total_loaded: i32 = [InventoryFlag::HiSlot1, InventoryFlag::HiSlot2, InventoryFlag::HiSlot3]
            .into_iter()
            .filter_map(|fl| {
                snap.items
                    .values()
                    .find(|i| i.location_id == ship_id && i.flag == fl && i.type_id == 83_901)
                    .map(|i| i.quantity)
            })
            .sum();
        assert_eq!(total_loaded, 2, "bank autoload must conserve exact total cargo quantity (2 rounds across 3 slots)");
    });
    let bank_notifs = ctx.drain_notifications_for_char(char_id);
    assert!(bank_notifs.iter().any(|n| n.event == "OnAmmoInBankChanged"));
}

fn setup_gh1124_space_pilot_and_ship(
    ctx: &ServerContext,
    char_id: i64,
    ship_id: i64,
    sys_id: u32,
    online_primary: bool,
) -> eve_machonet::SessionState {
    let uid = if online_primary {
        ctx.db.with_snapshot(|s| {
            s.accounts
                .values()
                .next()
                .expect("seed account")
                .user_id
        })
    } else {
        char_id as u64
    };
    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().cloned().expect("seed char");
        ch.character_id = char_id;
        ch.user_id = uid;
        ch.name = format!("Pilot {char_id}");
        ch.corporation_id = 1_000_009;
        ch.station_id = None;
        ch.structure_id = None;
        ch.solar_system_id = sys_id;
        ch.active_ship_id = ship_id;
        ch.security_status = 5.0;
        snap.characters.insert(char_id, ch);
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 587,
                owner_id: char_id,
                location_id: sys_id as i64,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });
    if online_primary {
        ctx.db
            .acquire_online_lock(uid, 1124, Some(char_id), true, ctx.now_unix())
            .expect("online lock");
    }
    let mut sess = eve_machonet::SessionState::new(1124);
    sess.user_id = Some(uid);
    sess.character_id = Some(char_id);
    sess.corporation_id = Some(1_000_009);
    sess.ship_id = Some(ship_id);
    sess.solar_system_id = Some(sys_id);
    sess
}

fn assert_no_reload_notifications(
    notifs: &[crate::router::PendingMachoNotification],
    label: &str,
) {
    assert!(
        !notifs.iter().any(|n| matches!(
            n.event.as_str(),
            "OnItemChange"
                | "OnGodmaPrimeItem"
                | "OnModuleAttributeChanges"
                | "OnChargeLoadedToModule"
                | "OnAmmoInBankChanged"
        )),
        "{label}: expected zero charge/reload notifications, got {:?}",
        notifs.iter().map(|n| n.event.as_str()).collect::<Vec<_>>()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn weapon_autoload_and_active_charge_enforce_consumability_and_skill_prereqs_gh1124() {
    use eve_proto::PyRep;

    let mut ctx = new_ctx();
    let sys_id = 30_000_142_u32;
    let char_id = 97_991_001_i64;
    let other_char_id = 97_991_002_i64;
    let ship_id = 97_991_100_i64;
    let target_ship_id = 97_991_101_i64;
    let turret_auto = 97_991_201_i64;
    let turret_preloaded = 97_991_202_i64;
    let preloaded_t2_charge = 97_991_203_i64;
    let foreign_cargo_ammo = 97_991_301_i64;
    let t2_cargo_ammo = 97_991_302_i64;
    let t1_cargo_ammo = 97_991_303_i64;
    let foreign_hold_ammo = 97_991_304_i64;
    let owned_hold_ammo = 97_991_305_i64;

    let mut ac_type = synth_type(
        55_911,
        55,
        "200mm AutoCannon I",
        &[10],
        &[(604, 83.0), (128, 1.0), (51, 2000.0), (54, 15_000.0), (6, 0.0)],
    );
    ac_type.capacity = 5.0;
    let mut t1_ammo = synth_type(83_911, 83, "Titanium Sabot S", &[], &[(128, 1.0), (114, 10.0)]);
    t1_ammo.category_id = 8;
    t1_ammo.volume = 0.0025;
    let mut t2_ammo = synth_type(
        83_912,
        83,
        "Barrage S",
        &[],
        &[(128, 1.0), (114, 15.0), (182, 12210.0), (277, 1.0)],
    );
    t2_ammo.category_id = 8;
    t2_ammo.volume = 0.0025;
    {
        let sde_mut = Arc::make_mut(&mut ctx.sde);
        sde_mut.types.insert(55_911, ac_type);
        sde_mut.types.insert(83_911, t1_ammo);
        sde_mut.types.insert(83_912, t2_ammo);
    }

    let mut sess = setup_gh1124_space_pilot_and_ship(&ctx, char_id, ship_id, sys_id, true);
    let _ = setup_gh1124_space_pilot_and_ship(&ctx, other_char_id, target_ship_id, sys_id, false);

    let sys = ctx.systems.get_or_spawn(sys_id);
    spawn_test_ball(&sys, ship_id, 587, DVec3::ZERO, MovementMode::Stop, 500.0).await;
    spawn_test_ball(&sys, target_ship_id, 587, DVec3::new(1_000.0, 0.0, 0.0), MovementMode::Stop, 500.0).await;
    let _ = sys
        .add_target_lock(
            ship_id,
            eve_destiny::TargetLock {
                target_id: target_ship_id,
                lock_time_ms: 1_000.0,
                remaining_lock_ms: 0.0,
                locked: true,
                max_range_m: None,
            },
        )
        .await;

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            turret_auto,
            InventoryItemRecord {
                item_id: turret_auto,
                type_id: 55_911,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            turret_preloaded,
            InventoryItemRecord {
                item_id: turret_preloaded,
                type_id: 55_911,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::HiSlot1,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            preloaded_t2_charge,
            InventoryItemRecord {
                item_id: preloaded_t2_charge,
                type_id: 83_912,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::HiSlot1,
                quantity: 5,
                singleton: false,
                ..Default::default()
            },
        );
        // Foreign-owned Cargo stack -> must be skipped by is_item_consumable_by
        snap.items.insert(
            foreign_cargo_ammo,
            InventoryItemRecord {
                item_id: foreign_cargo_ammo,
                type_id: 83_911,
                owner_id: other_char_id,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 10,
                singleton: false,
                ..Default::default()
            },
        );
    });
    ctx.invalidate_and_push_fit(ship_id);
    let _ = ctx.drain_notifications_for_char(char_id);

    // 1. Only wrong-owner ammo in Cargo -> rejected cleanly with NoAmmoLoaded
    let kw = HashMap::new();
    let err_foreign = crate::services::inventory_fitting::fitting_module_cycles::handle_module_cycles(
        &ctx,
        &mut sess,
        None,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(turret_auto), PyRep::String("targetAttack".into()), PyRep::Integer(target_ship_id)],
        &kw,
    )
    .await
    .expect("Some")
    .expect_err("wrong-owner ammo must not be consumable");
    assert_eq!(err_foreign.code, "NoAmmoLoaded");
    assert_eq!(ctx.db.with_snapshot(|s| s.items[&foreign_cargo_ammo].quantity), 10);
    assert_no_reload_notifications(&ctx.drain_notifications_for_char(char_id), "foreign-only cargo");

    // 2. Owned T2 ammo without required skill 12210 -> rejected cleanly with SkillPrerequisitesNotMet
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            t2_cargo_ammo,
            InventoryItemRecord {
                item_id: t2_cargo_ammo,
                type_id: 83_912,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 10,
                singleton: false,
                ..Default::default()
            },
        );
    });
    let _ = ctx.drain_notifications_for_char(char_id);
    let err_t2_skill = crate::services::inventory_fitting::fitting_module_cycles::handle_module_cycles(
        &ctx,
        &mut sess,
        None,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(turret_auto), PyRep::String("targetAttack".into()), PyRep::Integer(target_ship_id)],
        &kw,
    )
    .await
    .expect("Some")
    .expect_err("untrained T2 charge must fail skill prerequisite check");
    assert_eq!(err_t2_skill.code, "SkillPrerequisitesNotMet");
    ctx.db.with_snapshot(|s| {
        assert_eq!(s.items[&t2_cargo_ammo].quantity, 10, "untrained T2 cargo stack must remain untouched");
        assert!(
            !s.items.values().any(|i| i.location_id == ship_id && i.flag == InventoryFlag::HiSlot0 && i.item_id != turret_auto),
            "HiSlot0 must remain empty"
        );
    });
    assert_no_reload_notifications(&ctx.drain_notifications_for_char(char_id), "untrained T2 autoload");

    // 3. Preloaded untrained T2 charge in HiSlot1 -> rejected with SkillPrerequisitesNotMet and zero consumption
    let err_preloaded = crate::services::inventory_fitting::fitting_module_cycles::handle_module_cycles(
        &ctx,
        &mut sess,
        None,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(turret_preloaded), PyRep::String("targetAttack".into()), PyRep::Integer(target_ship_id)],
        &kw,
    )
    .await
    .expect("Some")
    .expect_err("preloaded untrained T2 charge must fail skill check");
    assert_eq!(err_preloaded.code, "SkillPrerequisitesNotMet");
    assert_eq!(ctx.db.with_snapshot(|s| s.items[&preloaded_t2_charge].quantity), 5);
    assert_no_reload_notifications(&ctx.drain_notifications_for_char(char_id), "untrained preloaded T2");

    // 4. Trained T1 stack added with higher item_id alongside untrained T2 stack -> autoloads trained T1 stack cleanly
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            t1_cargo_ammo,
            InventoryItemRecord {
                item_id: t1_cargo_ammo,
                type_id: 83_911,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 3,
                singleton: false,
                ..Default::default()
            },
        );
    });
    crate::services::inventory_fitting::fitting_module_cycles::handle_module_cycles(
        &ctx,
        &mut sess,
        None,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(turret_auto), PyRep::String("targetAttack".into()), PyRep::Integer(target_ship_id)],
        &kw,
    )
    .await
    .expect("Some")
    .expect("trained T1 fallback should autoload and fire");
    ctx.db.with_snapshot(|s| {
        assert_eq!(s.items[&t2_cargo_ammo].quantity, 10, "untrained T2 stack untouched");
        assert!(!s.items.contains_key(&t1_cargo_ammo), "trained T1 cargo stack consumed on autoload");
        let loaded_t1 = s
            .items
            .values()
            .find(|i| i.location_id == ship_id && i.flag == InventoryFlag::HiSlot0 && i.item_id != turret_auto)
            .expect("T1 ammo loaded into HiSlot0");
        assert_eq!(loaded_t1.type_id, 83_911);
        assert_eq!(loaded_t1.quantity, 2, "3 loaded then 1 round fired on Activate -> 2 remaining");
    });

    // 5. SpecializedAmmoHold: wrong-owner stack skipped, owned trained stack loaded into a fresh turret
    let turret_hold = 97_991_204_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.remove(&t2_cargo_ammo);
        snap.items.insert(
            turret_hold,
            InventoryItemRecord {
                item_id: turret_hold,
                type_id: 55_911,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::HiSlot2,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            foreign_hold_ammo,
            InventoryItemRecord {
                item_id: foreign_hold_ammo,
                type_id: 83_911,
                owner_id: other_char_id,
                location_id: ship_id,
                flag: InventoryFlag::SpecializedAmmoHold,
                quantity: 5,
                singleton: false,
                ..Default::default()
            },
        );
        snap.items.insert(
            owned_hold_ammo,
            InventoryItemRecord {
                item_id: owned_hold_ammo,
                type_id: 83_911,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::SpecializedAmmoHold,
                quantity: 2,
                singleton: false,
                ..Default::default()
            },
        );
    });
    ctx.invalidate_and_push_fit(ship_id);
    let _ = ctx.drain_notifications_for_char(char_id);
    crate::services::inventory_fitting::fitting_module_cycles::handle_module_cycles(
        &ctx,
        &mut sess,
        None,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(turret_hold), PyRep::String("targetAttack".into()), PyRep::Integer(target_ship_id)],
        &kw,
    )
    .await
    .expect("Some")
    .expect("owned SpecializedAmmoHold stack should autoload after skipping foreign stack");
    ctx.db.with_snapshot(|s| {
        assert_eq!(s.items[&foreign_hold_ammo].quantity, 5, "foreign ammo-hold stack untouched");
        assert!(!s.items.contains_key(&owned_hold_ammo), "owned ammo-hold stack consumed");
        let loaded = s
            .items
            .values()
            .find(|i| i.location_id == ship_id && i.flag == InventoryFlag::HiSlot2 && i.item_id != turret_hold)
            .expect("ammo-hold charge loaded into HiSlot2");
        assert_eq!(loaded.quantity, 1, "2 loaded then 1 round fired -> 1 remaining");
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn weapon_autoload_defers_commit_on_partial_missile_bank_and_precondition_failures_gh1124() {
    use crate::services::combat_crimewatch::{set_char_safety_level, SafetyLevel};
    use eve_proto::PyRep;

    let mut ctx = new_ctx();
    let sys_id = 30_000_142_u32;
    let char_id = 97_992_001_i64;
    let target_char_id = 97_992_002_i64;
    let ship_id = 97_992_100_i64;
    let target_ship_id = 97_992_101_i64;
    let far_ship_id = 97_992_102_i64;

    let m_bank_1 = 97_992_201_i64;
    let m_bank_2 = 97_992_202_i64;
    let m_bank_3 = 97_992_203_i64;
    let turret_id = 97_992_210_i64;
    let cargo_missiles = 97_992_301_i64;
    let cargo_bullets = 97_992_302_i64;

    let mut launcher_type = synth_type(
        50_901,
        509,
        "Light Missile Launcher I",
        &[101],
        &[(604, 384.0), (51, 2000.0), (6, 0.0)],
    );
    launcher_type.capacity = 5.0;
    let mut ac_type = synth_type(
        55_921,
        55,
        "200mm AutoCannon I",
        &[10],
        &[(604, 83.0), (128, 1.0), (51, 2000.0), (54, 15_000.0), (6, 25.0)],
    );
    ac_type.capacity = 5.0;
    let mut missile_type = synth_type(
        38_401,
        384,
        "Scourge Light Missile",
        &[],
        &[(37, 2000.0), (281, 5000.0), (117, 20.0)],
    );
    missile_type.category_id = 8;
    missile_type.volume = 0.01;
    let mut bullet_type = synth_type(83_921, 83, "Titanium Sabot S", &[], &[(128, 1.0), (114, 10.0)]);
    bullet_type.category_id = 8;
    bullet_type.volume = 0.0025;
    {
        let sde_mut = Arc::make_mut(&mut ctx.sde);
        sde_mut.types.insert(50_901, launcher_type);
        sde_mut.types.insert(55_921, ac_type);
        sde_mut.types.insert(38_401, missile_type);
        sde_mut.types.insert(83_921, bullet_type);
    }

    let mut sess = setup_gh1124_space_pilot_and_ship(&ctx, char_id, ship_id, sys_id, true);
    let _ = setup_gh1124_space_pilot_and_ship(&ctx, target_char_id, target_ship_id, sys_id, false);

    let sys = ctx.systems.get_or_spawn(sys_id);
    spawn_test_ball(&sys, ship_id, 587, DVec3::ZERO, MovementMode::Stop, 500.0).await;
    spawn_test_ball(&sys, target_ship_id, 587, DVec3::new(1_000.0, 0.0, 0.0), MovementMode::Stop, 500.0).await;
    spawn_test_ball(&sys, far_ship_id, 587, DVec3::new(500_000.0, 0.0, 0.0), MovementMode::Stop, 500.0).await;
    for locked_tid in [target_ship_id, far_ship_id] {
        let _ = sys
            .add_target_lock(
                ship_id,
                eve_destiny::TargetLock {
                    target_id: locked_tid,
                    lock_time_ms: 1_000.0,
                    remaining_lock_ms: 0.0,
                    locked: true,
                    max_range_m: None,
                },
            )
            .await;
    }

    ctx.db.with_mut_snapshot(|snap| {
        for (mid, fl) in [
            (m_bank_1, InventoryFlag::HiSlot0),
            (m_bank_2, InventoryFlag::HiSlot1),
            (m_bank_3, InventoryFlag::HiSlot2),
        ] {
            snap.items.insert(
                mid,
                InventoryItemRecord {
                    item_id: mid,
                    type_id: 50_901,
                    owner_id: char_id,
                    location_id: ship_id,
                    flag: fl,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
        }
        snap.weapon_banks
            .entry(ship_id)
            .or_default()
            .insert(m_bank_1, vec![m_bank_1, m_bank_2, m_bank_3]);
        snap.items.insert(
            cargo_missiles,
            InventoryItemRecord {
                item_id: cargo_missiles,
                type_id: 38_401,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 2,
                singleton: false,
                ..Default::default()
            },
        );
    });
    ctx.invalidate_and_push_fit(ship_id);
    let _ = ctx.drain_notifications_for_char(char_id);

    let kw = HashMap::new();
    let assert_clean_missile_bank_state = |expected_qty: i32, label: &str| {
        ctx.db.with_snapshot(|s| {
            assert_eq!(s.items[&cargo_missiles].quantity, expected_qty, "{label}: cargo_missiles mutated");
            assert!(
                !s.items.values().any(|i| {
                    i.location_id == ship_id
                        && matches!(i.flag, InventoryFlag::HiSlot0 | InventoryFlag::HiSlot1 | InventoryFlag::HiSlot2)
                        && ![m_bank_1, m_bank_2, m_bank_3].contains(&i.item_id)
                }),
                "{label}: slot charges must not be partially loaded"
            );
            assert!(
                !s.module_active_charges.contains_key(&m_bank_1)
                    && !s.module_active_charges.contains_key(&m_bank_2)
                    && !s.module_active_charges.contains_key(&m_bank_3),
                "{label}: module_active_charges must remain empty"
            );
        });
    };

    // 1. Partial grouped missile bank (2 missiles for 3 launchers) -> NoCharges, zero side effects
    let err_partial = crate::services::inventory_fitting::fitting_module_cycles::handle_module_cycles(
        &ctx,
        &mut sess,
        None,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(m_bank_1), PyRep::String("useMissiles".into()), PyRep::Integer(target_ship_id)],
        &kw,
    )
    .await
    .expect("Some")
    .expect_err("partial grouped missile bank must fail");
    assert_eq!(err_partial.code, "NoCharges");
    assert_clean_missile_bank_state(2, "partial missile bank");
    assert_no_reload_notifications(&ctx.drain_notifications_for_char(char_id), "partial missile bank");

    // 2. Enough missiles (10) for full 3-launcher bank, but TargetTooFar (500 km vs 10 km flight range) -> zero side effects
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(st) = snap.items.get_mut(&cargo_missiles) {
            st.quantity = 10;
        }
    });
    let _ = ctx.drain_notifications_for_char(char_id);
    let err_range = crate::services::inventory_fitting::fitting_module_cycles::handle_module_cycles(
        &ctx,
        &mut sess,
        None,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(m_bank_1), PyRep::String("useMissiles".into()), PyRep::Integer(far_ship_id)],
        &kw,
    )
    .await
    .expect("Some")
    .expect_err("out-of-range missile bank must fail");
    assert_eq!(err_range.code, "TargetTooFar");
    assert_clean_missile_bank_state(10, "TargetTooFar");
    assert_no_reload_notifications(&ctx.drain_notifications_for_char(char_id), "TargetTooFar");

    // Replace HiSlot0..2 with single capacitor-consuming turret + cargo_bullets for cap / target-lock / safety failure checks
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.remove(&m_bank_1);
        snap.items.remove(&m_bank_2);
        snap.items.remove(&m_bank_3);
        snap.items.remove(&cargo_missiles);
        snap.weapon_banks.remove(&ship_id);
        snap.items.insert(
            turret_id,
            InventoryItemRecord {
                item_id: turret_id,
                type_id: 55_921,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            cargo_bullets,
            InventoryItemRecord {
                item_id: cargo_bullets,
                type_id: 83_921,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 10,
                singleton: false,
                ..Default::default()
            },
        );
    });
    ctx.invalidate_and_push_fit(ship_id);
    let _ = ctx.drain_notifications_for_char(char_id);

    let assert_clean_turret_state = |label: &str| {
        ctx.db.with_snapshot(|s| {
            assert_eq!(s.items[&cargo_bullets].quantity, 10, "{label}: cargo_bullets mutated");
            assert!(
                !s.items.values().any(|i| i.location_id == ship_id && i.flag == InventoryFlag::HiSlot0 && i.item_id != turret_id),
                "{label}: turret slot must remain empty"
            );
            assert!(
                !s.module_active_charges.contains_key(&turret_id),
                "{label}: module_active_charges must remain empty"
            );
        });
    };

    // 3. TargetNotLocked -> zero side effects
    let unlocked_target = 97_992_999_i64;
    let err_unlocked = crate::services::inventory_fitting::fitting_module_cycles::handle_module_cycles(
        &ctx,
        &mut sess,
        None,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(turret_id), PyRep::String("targetAttack".into()), PyRep::Integer(unlocked_target)],
        &kw,
    )
    .await
    .expect("Some")
    .expect_err("unlocked target must fail");
    assert_eq!(err_unlocked.code, "TargetNotLocked");
    assert_clean_turret_state("TargetNotLocked");
    assert_no_reload_notifications(&ctx.drain_notifications_for_char(char_id), "TargetNotLocked");

    // 4. NotEnoughCapacitor -> zero side effects
    let _ = sys.set_capacitor(ship_id, 0.0).await;
    let err_cap = crate::services::inventory_fitting::fitting_module_cycles::handle_module_cycles(
        &ctx,
        &mut sess,
        None,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(turret_id), PyRep::String("targetAttack".into()), PyRep::Integer(target_ship_id)],
        &kw,
    )
    .await
    .expect("Some")
    .expect_err("zero capacitor must fail");
    assert_eq!(err_cap.code, "NotEnoughCapacitor");
    assert_clean_turret_state("NotEnoughCapacitor");
    assert_no_reload_notifications(&ctx.drain_notifications_for_char(char_id), "NotEnoughCapacitor");

    // 5. Crimewatch SafetyLevel::Green in Jita -> CantDoInCurrentSafetyLevel, zero side effects
    let _ = sys.set_capacitor(ship_id, 500.0).await;
    set_char_safety_level(&ctx, char_id, SafetyLevel::Green);
    let err_safety = crate::services::inventory_fitting::fitting_module_cycles::handle_module_cycles(
        &ctx,
        &mut sess,
        None,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(turret_id), PyRep::String("targetAttack".into()), PyRep::Integer(target_ship_id)],
        &kw,
    )
    .await
    .expect("Some")
    .expect_err("Safety Green aggression in Jita must fail");
    assert_eq!(err_safety.code, "CantDoInCurrentSafetyLevel");
    assert_clean_turret_state("CantDoInCurrentSafetyLevel");
    assert_no_reload_notifications(&ctx.drain_notifications_for_char(char_id), "CantDoInCurrentSafetyLevel");

    // 6. SafetyLevel::Red -> autoload + first round commit cleanly after preconditions pass
    set_char_safety_level(&ctx, char_id, SafetyLevel::Red);
    crate::services::inventory_fitting::fitting_module_cycles::handle_module_cycles(
        &ctx,
        &mut sess,
        None,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(turret_id), PyRep::String("targetAttack".into()), PyRep::Integer(target_ship_id)],
        &kw,
    )
    .await
    .expect("Some")
    .expect("Safety Red activation must succeed");
    ctx.db.with_snapshot(|s| {
        assert!(!s.items.contains_key(&cargo_bullets), "cargo_bullets loaded into slot");
        let slot_charge = s
            .items
            .values()
            .find(|i| i.location_id == ship_id && i.flag == InventoryFlag::HiSlot0 && i.item_id != turret_id)
            .expect("slot charge created");
        assert_eq!(slot_charge.quantity, 9, "10 loaded then 1 round fired -> 9 remaining");
    });
    let ok_notifs = ctx.drain_notifications_for_char(char_id);
    assert!(ok_notifs.iter().any(|n| n.event == "OnGodmaPrimeItem"));
    assert!(ok_notifs.iter().any(|n| n.event == "OnModuleAttributeChanges"));
    assert!(ok_notifs.iter().any(|n| n.event == "OnChargeLoadedToModule"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn weapon_autoload_damaged_crystal_preserves_dynamic_attributes_and_mutaplasmid_gh1124() {
    use eve_proto::PyRep;

    let mut ctx = new_ctx();
    let sys_id = 30_000_142_u32;
    let char_id = 97_993_001_i64;
    let other_char_id = 97_993_002_i64;
    let ship_id = 97_993_100_i64;
    let target_ship_id = 97_993_101_i64;
    let laser_singleton_id = 97_993_201_i64;
    let laser_split_id = 97_993_202_i64;
    let laser_fastpath_id = 97_993_203_i64;
    let crys_singleton_id = 97_993_301_i64;
    let crys_split_id = 97_993_302_i64;
    let crys_fastpath_id = 97_993_303_i64;

    let mut laser_type = synth_type(
        53_912,
        53,
        "Pulse Laser II",
        &[10],
        &[(604, 86.0), (128, 1.0), (51, 3000.0), (54, 15_000.0), (158, 5_000.0), (160, 1.0), (6, 5.0), (64, 1.0)],
    );
    laser_type.capacity = 5.0;
    let mut crystal_type = synth_type(
        86_912,
        86,
        "Scorch S",
        &[],
        &[
            (128, 1.0),
            (114, 25.0),
            (118, 25.0),
            (786, 1.0),
            (783, 1.0),
            (784, 0.05),
            (9, 1.0),
        ],
    );
    crystal_type.category_id = 8;
    crystal_type.volume = 1.0;
    {
        let sde_mut = Arc::make_mut(&mut ctx.sde);
        sde_mut.types.insert(53_912, laser_type);
        sde_mut.types.insert(86_912, crystal_type);
    }

    let mut sess = setup_gh1124_space_pilot_and_ship(&ctx, char_id, ship_id, sys_id, true);
    let _ = setup_gh1124_space_pilot_and_ship(&ctx, other_char_id, target_ship_id, sys_id, false);
    let sys = ctx.systems.get_or_spawn(sys_id);
    spawn_test_ball(&sys, ship_id, 587, DVec3::ZERO, MovementMode::Stop, 500.0).await;
    spawn_test_ball(&sys, target_ship_id, 587, DVec3::new(1_000.0, 0.0, 0.0), MovementMode::Stop, 500.0).await;
    let _ = sys
        .add_target_lock(
            ship_id,
            eve_destiny::TargetLock {
                target_id: target_ship_id,
                lock_time_ms: 1_000.0,
                remaining_lock_ms: 0.0,
                locked: true,
                max_range_m: None,
            },
        )
        .await;

    // 1. Move-in-place branch (`singleton: true`): damaged crystal keeps damage + source_mutaplasmid_id
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            laser_singleton_id,
            InventoryItemRecord {
                item_id: laser_singleton_id,
                type_id: 53_912,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            laser_split_id,
            InventoryItemRecord {
                item_id: laser_split_id,
                type_id: 53_912,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::HiSlot1,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            laser_fastpath_id,
            InventoryItemRecord {
                item_id: laser_fastpath_id,
                type_id: 53_912,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::HiSlot2,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        let mut s_dyn = HashMap::new();
        s_dyn.insert(eve_sde::attr_ids::DAMAGE, 0.45);
        snap.items.insert(
            crys_singleton_id,
            InventoryItemRecord {
                item_id: crys_singleton_id,
                type_id: 86_912,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                singleton: true,
                dynamic_attributes: s_dyn,
                source_mutaplasmid_id: Some(49_711),
                ..Default::default()
            },
        );
    });
    ctx.invalidate_and_push_fit(ship_id);
    let kw = HashMap::new();
    crate::services::inventory_fitting::fitting_module_cycles::handle_module_cycles(
        &ctx,
        &mut sess,
        None,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(laser_singleton_id), PyRep::String("targetAttack".into()), PyRep::Integer(target_ship_id)],
        &kw,
    )
    .await
    .expect("Some")
    .expect("singleton damaged crystal autoload must succeed");
    ctx.db.with_snapshot(|s| {
        let loaded = &s.items[&crys_singleton_id];
        assert_eq!(loaded.flag, InventoryFlag::HiSlot0);
        assert!(loaded.singleton);
        assert_eq!(loaded.dynamic_attributes.get(&eve_sde::attr_ids::DAMAGE), Some(&0.45));
        assert_eq!(loaded.source_mutaplasmid_id, Some(49_711));
    });

    // 2. Stack-split branch (`singleton: false`): damaged non-singleton crystal keeps damage + source_mutaplasmid_id
    ctx.db.with_mut_snapshot(|snap| {
        let mut ns_dyn = HashMap::new();
        ns_dyn.insert(eve_sde::attr_ids::DAMAGE, 0.62);
        snap.items.insert(
            crys_split_id,
            InventoryItemRecord {
                item_id: crys_split_id,
                type_id: 86_912,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                singleton: false,
                dynamic_attributes: ns_dyn,
                source_mutaplasmid_id: Some(49_712),
                ..Default::default()
            },
        );
    });
    crate::services::inventory_fitting::fitting_module_cycles::handle_module_cycles(
        &ctx,
        &mut sess,
        None,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(laser_split_id), PyRep::String("targetAttack".into()), PyRep::Integer(target_ship_id)],
        &kw,
    )
    .await
    .expect("Some")
    .expect("non-singleton damaged crystal autoload must succeed");
    ctx.db.with_snapshot(|s| {
        let loaded = s
            .items
            .values()
            .find(|i| i.location_id == ship_id && i.flag == InventoryFlag::HiSlot1 && i.item_id != laser_split_id)
            .expect("split damaged crystal loaded into HiSlot1");
        assert_eq!(loaded.dynamic_attributes.get(&eve_sde::attr_ids::DAMAGE), Some(&0.62));
        assert_eq!(loaded.source_mutaplasmid_id, Some(49_712));
    });

    // 3. Direct `combat_weapons::handle_weapons` autoload preserves damaged crystal & advances volatility from 0.40 -> 0.45
    ctx.db.with_mut_snapshot(|snap| {
        let mut fp_dyn = HashMap::new();
        fp_dyn.insert(eve_sde::attr_ids::DAMAGE, 0.40);
        snap.items.insert(
            crys_fastpath_id,
            InventoryItemRecord {
                item_id: crys_fastpath_id,
                type_id: 86_912,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                singleton: false,
                dynamic_attributes: fp_dyn,
                source_mutaplasmid_id: Some(49_713),
                ..Default::default()
            },
        );
    });
    let res = crate::services::combat_crimewatch::combat_weapons::handle_weapons(
        &ctx,
        &mut sess,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(laser_fastpath_id), PyRep::String("targetAttack".into()), PyRep::Integer(target_ship_id)],
        &kw,
    )
    .await
    .expect("Some")
    .expect("handle_weapons damaged crystal autoload + fire must succeed");
    let frac = match &res {
        PyRep::KeyVal(entries) => entries.get("crystalDamageFraction").and_then(|v| v.as_f64()),
        _ => None,
    }
    .expect("crystalDamageFraction in response");
    assert!((frac - 0.45).abs() < 1e-6, "expected 0.40 + 0.05 = 0.45 damage, got {frac}");
    ctx.db.with_snapshot(|s| {
        let loaded = s
            .items
            .values()
            .find(|i| i.location_id == ship_id && i.flag == InventoryFlag::HiSlot2 && i.item_id != laser_fastpath_id)
            .expect("fastpath crystal loaded into HiSlot2");
        let dmg = loaded.dynamic_attributes.get(&eve_sde::attr_ids::DAMAGE).copied().unwrap_or(0.0);
        assert!((dmg - 0.45).abs() < 1e-6, "slot crystal damage must advance from 0.40 to 0.45, got {dmg}");
        assert_eq!(loaded.source_mutaplasmid_id, Some(49_713));
    });
}
