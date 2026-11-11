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
    let bas_ok = run_prep(
        &ctx, &sys, sys_id, char_id, ship_id, 900, mod_id, &bas_fm, &bas_type, &bas_st, &fit, None, false, false, false,
    ).await.unwrap();
    assert!(bas_ok.immobilize_self && !bas_ok.self_ewar_immune);
    let wep_until = ctx.crimewatch.write().unwrap().get_or_create(char_id).weapon_timer_until_unix;
    assert!(wep_until >= ctx.now_unix_secs() + 60);

    // Phenomena Generator (group 1815) hull check, partial fuel removal, and dbuff application
    let (ph_fm, ph_st) = synth_module_and_state(mod_id, 43554, 27, None, &[(2468, 9901.0), (2469, 25.0)]);
    let ph_type = synth_type(43554, 1815, "Amarr Phenomena Generator", &[6753], &[(713, 16274.0), (714, 500.0)]);
    assert_eq!(
        run_prep(&ctx, &sys, sys_id, char_id, ship_id, 27, mod_id, &ph_fm, &ph_type, &ph_st, &fit, None, false, false, false)
            .await.err().unwrap().code,
        "InvalidShipGroup"
    );
    let ph_ok = run_prep(
        &ctx, &sys, sys_id, char_id, ship_id, 30, mod_id, &ph_fm, &ph_type, &ph_st, &fit, None, false, false, false,
    ).await.unwrap();
    assert!(ph_ok.immobilize_self);
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
    def_short.fuel_to_deduct = Some((fuel_tid, 5, false));
    commit_post_cycle_activation_effects(&ctx, &sys, char_id, ship_id, mod_id, &tp_fm, &tp_type, &tp_st_25, None, def_short)
        .await
        .unwrap();
    assert_eq!(ctx.db.with_snapshot(|s| s.items[&fuel_id].quantity), 4);

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&fuel_id).unwrap().quantity = 5;
    });
    let mut def_exact = DeferredPostCycleEffects::default();
    def_exact.fuel_to_deduct = Some((fuel_tid, 5, false));
    commit_post_cycle_activation_effects(&ctx, &sys, char_id, ship_id, mod_id, &tp_fm, &tp_type, &tp_st_25, None, def_exact)
        .await
        .unwrap();
    assert!(ctx.db.with_snapshot(|s| !s.items.contains_key(&fuel_id)));
}
