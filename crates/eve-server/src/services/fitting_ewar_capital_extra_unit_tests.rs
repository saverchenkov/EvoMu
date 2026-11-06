//! Deep branch-coverage and mutation-killing unit tests for `fitting_ewar_capital`.

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::inventory_fitting::fitting_ewar_capital::{
    emit_committed_ewar_and_system_wide_notifications, ewar_jamming_type,
    prepare_ewar_and_capital_module, EwarCapitalPrep,
};
use eve_db::{
    AccountRecord, InventoryFlag, InventoryItemRecord, TrainedSkillRecord, WorldRepository,
};
use eve_destiny::{Ball, CombatState, DVec3, MovementMode, SlimItemMeta, WarpPhase};
use eve_dogma::{attr_ids, DogmaItemState, FittedModule, ModuleState, ShipFittingEngine};
use eve_proto::{BridgeRpcError, PyRep};
use eve_sde::{DbuffCollectionInfo, InvType, SdeRegistry};
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
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

fn new_ctx() -> ServerContext {
    ServerContext::with_sde_and_repository(shared_sde(), Arc::new(WorldRepository::with_demo_seed()))
}

fn synth_type(type_id: u32, group_id: u32, name: &str, effects: &[u16], attrs: &[(u16, f64)]) -> InvType {
    let mut t = shared_sde().types.get(&587).expect("seed type").clone();
    t.type_id = type_id; t.group_id = group_id; t.category_id = 7; t.name = name.into();
    t.effect_ids = effects.to_vec(); t.default_attributes.clear();
    for &(k, v) in attrs { t.default_attributes.insert(k, v); }
    t
}

fn synth_module_and_state(
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
async fn run_prep(
    ctx: &ServerContext, sys: &crate::system_actor::SystemHandle, sys_id: u32, char_id: i64,
    ship_id: i64, ship_group_id: u32, module_id: i64, fm: &FittedModule, mod_type: &InvType,
    mod_state: &DogmaItemState, fit: &ShipFittingEngine, target_id_opt: Option<i64>,
    is_overloaded: bool, is_mwd: bool, is_mjd: bool,
) -> Result<EwarCapitalPrep, BridgeRpcError> {
    prepare_ewar_and_capital_module(
        ctx, sys, sys_id, char_id, ship_id, ship_group_id, module_id, fm, mod_type, mod_state,
        fit, target_id_opt, is_overloaded, is_mwd, is_mjd, &HashMap::new(),
    )
    .await
    .expect("Some")
}

async fn spawn_test_ball(
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

#[tokio::test]
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

#[tokio::test]
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
    assert!(ctx.db.with_snapshot(|s| s.items[&victim_id].dynamic_attributes.contains_key(&37)));

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

#[tokio::test]
async fn scram_compressor_wdfg_wcs_ewar_grappler_remote_assist_and_burst_projectors() {
    let ctx = new_ctx();
    let sys_id = 30_000_142u32;
    let sys = ctx.systems.get_or_spawn(sys_id);
    let char_id = 97_100_003i64;
    let ship_id = 97_100_104i64;
    let target_id = 97_100_105i64;
    let mod_id = 97_100_204i64;
    let fit = ShipFittingEngine::from_sde(ship_id, 587, &ctx.sde);

    spawn_test_ball(&sys, ship_id, 587, DVec3::ZERO, MovementMode::Stop, 1000.0).await;
    spawn_test_ball(&sys, target_id, 587, DVec3::new(15_000.0, 0.0, 0.0), MovementMode::Stop, 5000.0).await;

    // 1. Warp Scrambler (group 52): missing target, self target, out of range, overloaded in range
    let (scram_fm, scram_st) = synth_module_and_state(mod_id, 448, 19, None, &[(attr_ids::MAX_RANGE, 14_000.0)]);
    let scram_type = synth_type(448, 52, "Warp Scrambler II", &[], &[(1208, 20.0)]);
    assert_eq!(
        run_prep(&ctx, &sys, sys_id, char_id, ship_id, 25, mod_id, &scram_fm, &scram_type, &scram_st, &fit, None, false, false, false)
            .await.err().unwrap().code,
        "TargetRequired"
    );
    assert_eq!(
        run_prep(&ctx, &sys, sys_id, char_id, ship_id, 25, mod_id, &scram_fm, &scram_type, &scram_st, &fit, Some(ship_id), false, false, false)
            .await.err().unwrap().code,
        "CannotActivateOnSelf"
    );
    assert_eq!(
        run_prep(&ctx, &sys, sys_id, char_id, ship_id, 25, mod_id, &scram_fm, &scram_type, &scram_st, &fit, Some(target_id), false, false, false)
            .await.err().unwrap().code,
        "TargetOutOfRange"
    );
    // Overloaded increases 14km * 1.2 = 16.8km >= 15km
    let scram_ok = run_prep(
        &ctx, &sys, sys_id, char_id, ship_id, 25, mod_id, &scram_fm, &scram_type, &scram_st, &fit, Some(target_id), true, false, false,
    ).await.unwrap();
    assert!(scram_ok.warp_scramble_points >= 1);

    // 2. Shipboard Compressor (attr 3265 > 0.0) requires active Industrial Core (group 515)
    let (comp_fm, comp_st) = synth_module_and_state(mod_id, 62000, 27, None, &[]);
    let comp_type = synth_type(62000, 4174, "Medium Industrial Compressor I", &[], &[(3265, 1.0)]);
    assert_eq!(
        run_prep(&ctx, &sys, sys_id, char_id, ship_id, 941, mod_id, &comp_fm, &comp_type, &comp_st, &fit, None, false, false, false)
            .await.err().unwrap().code,
        "IndustrialCoreNotActive"
    );
    let mut fit_with_core = fit.clone();
    fit_with_core.modules.insert(
        999_002,
        FittedModule {
            item_id: 999_002,
            type_id: 28583,
            flag_id: 28,
            state: ModuleState::Active,
            charge_type_id: None,
            charge_quantity: 0,
            damage_hp: 0.0,
            max_hp: 40.0,
            dynamic_attribute_overrides: HashMap::new(),
        },
    );
    assert!(run_prep(
        &ctx, &sys, sys_id, char_id, ship_id, 941, mod_id, &comp_fm, &comp_type, &comp_st, &fit_with_core, None, false, false, false,
    ).await.is_ok());

    // 3. HIC WDFG (group 899): invalid hull, scripted missing target, scripted out of range, scripted ok, unscripted Zarzakh vs normal
    let (wdfg_fm, wdfg_st) = synth_module_and_state(mod_id, 28654, 27, Some(45010), &[]);
    let wdfg_type = synth_type(28654, 899, "Warp Disruption Field Generator I", &[], &[(103, 10_000.0), (5561, 1.0)]);
    assert_eq!(
        run_prep(&ctx, &sys, sys_id, char_id, ship_id, 25, mod_id, &wdfg_fm, &wdfg_type, &wdfg_st, &fit, Some(target_id), false, false, false)
            .await.err().unwrap().code,
        "InvalidShipGroup"
    );
    assert_eq!(
        run_prep(&ctx, &sys, sys_id, char_id, ship_id, 894, mod_id, &wdfg_fm, &wdfg_type, &wdfg_st, &fit, None, false, false, false)
            .await.err().unwrap().code,
        "TargetRequired"
    );
    // Script 45010 gives 1.2 * 10km = 12km < 15km -> TargetOutOfRange; overloaded gives 1.2 * 1.2 * 10km = 14.4km; script 29003 gives 1.5 * 10km = 15km
    assert_eq!(
        run_prep(&ctx, &sys, sys_id, char_id, ship_id, 894, mod_id, &wdfg_fm, &wdfg_type, &wdfg_st, &fit, Some(target_id), false, false, false)
            .await.err().unwrap().code,
        "TargetOutOfRange"
    );
    let (wdfg_fm_29003, _) = synth_module_and_state(mod_id, 28654, 27, Some(29003), &[]);
    let wdfg_scr_ok = run_prep(
        &ctx, &sys, sys_id, char_id, ship_id, 894, mod_id, &wdfg_fm_29003, &wdfg_type, &wdfg_st, &fit, Some(target_id), false, false, false,
    ).await.unwrap();
    assert_eq!(wdfg_scr_ok.warp_scramble_points, 100);
    assert!(wdfg_scr_ok.disables_target_mwd_mjd && wdfg_scr_ok.hic_bubble_radius_m.is_none());

    let (wdfg_unscr, _) = synth_module_and_state(mod_id, 28654, 27, None, &[]);
    assert_eq!(
        run_prep(&ctx, &sys, 30_100_000, char_id, ship_id, 894, mod_id, &wdfg_unscr, &wdfg_type, &wdfg_st, &fit, None, false, false, false)
            .await.err().unwrap().code,
        "ModuleDisallowedInHazardSystem"
    );
    let wdfg_bub_ok = run_prep(
        &ctx, &sys, sys_id, char_id, ship_id, 894, mod_id, &wdfg_unscr, &wdfg_type, &wdfg_st, &fit, None, false, false, false,
    ).await.unwrap();
    assert_eq!(wdfg_bub_ok.hic_bubble_radius_m, Some(10_000.0));
    assert_eq!(wdfg_bub_ok.hic_velocity_multiplier, Some(0.40));

    // 4. Warp Core Stabilizer (group 315)
    let wcs_type = synth_type(10998, 315, "Warp Core Stabilizer I", &[], &[(105, -2.0)]);
    let wcs_ok = run_prep(
        &ctx, &sys, sys_id, char_id, ship_id, 25, mod_id, &wdfg_unscr, &wcs_type, &wdfg_st, &fit, None, false, false, false,
    ).await.unwrap();
    assert_eq!(wcs_ok.warp_core_stab_bonus, 2);

    // 5. Webifier (65), Grappler (1672), Target Painter (379)
    let (web_fm, web_st) = synth_module_and_state(
        mod_id,
        526,
        19,
        None,
        &[(attr_ids::MAX_RANGE, 10_000.0), (attr_ids::FALLOFF, 0.0), (2044, 0.0), (20, -60.0)],
    );
    let web_type = synth_type(526, 65, "Stasis Webifier I", &[], &[]);
    assert_eq!(
        run_prep(&ctx, &sys, sys_id, char_id, ship_id, 25, mod_id, &web_fm, &web_type, &web_st, &fit, None, false, false, false)
            .await.err().unwrap().code,
        "TargetRequired"
    );
    assert_eq!(
        run_prep(&ctx, &sys, sys_id, char_id, ship_id, 25, mod_id, &web_fm, &web_type, &web_st, &fit, Some(ship_id), false, false, false)
            .await.err().unwrap().code,
        "CannotActivateOnSelf"
    );
    assert_eq!(
        run_prep(&ctx, &sys, sys_id, char_id, ship_id, 25, mod_id, &web_fm, &web_type, &web_st, &fit, Some(target_id), false, false, false)
            .await.err().unwrap().code,
        "TargetOutOfRange"
    );

    let (grap_fm, grap_st) = synth_module_and_state(
        mod_id,
        40690,
        19,
        None,
        &[(attr_ids::MAX_RANGE, 1_000.0), (attr_ids::FALLOFF, 9_000.0), (20, -85.0)],
    );
    let grap_type = synth_type(40690, 1672, "Heavy Stasis Grappler I", &[], &[]);
    let grap_ok = run_prep(
        &ctx, &sys, sys_id, char_id, ship_id, 27, mod_id, &grap_fm, &grap_type, &grap_st, &fit, Some(target_id), false, false, false,
    ).await.unwrap();
    assert_eq!(grap_ok.grappler_optimal_m, 1_000.0);
    assert_eq!(grap_ok.grappler_falloff_m, 9_000.0);
    assert!(grap_ok.target_velocity_multiplier.unwrap() < 1.0);

    let tp_type = synth_type(12709, 379, "Target Painter I", &[], &[]);
    let _ = run_prep(
        &ctx, &sys, sys_id, char_id, ship_id, 25, mod_id, &grap_fm, &tp_type, &grap_st, &fit, Some(target_id), false, false, false,
    ).await.unwrap();

    // 6. Remote Assistance (41, 325, 2018, 585, 67)
    let (rr_fm, rr_st) = synth_module_and_state(
        mod_id,
        49722,
        27,
        None,
        &[(attr_ids::MAX_RANGE, 30_000.0), (68, 200.0), (84, 300.0), (83, 150.0), (90, 120.0)],
    );
    let mut muta_fm = rr_fm.clone();
    muta_fm.dynamic_attribute_overrides.insert(84, 450.0);
    let muta_type = synth_type(49722, 2018, "Mutadaptive Remote Armor Repairer", &[], &[(2796, 0.15), (2797, 2.50)]);
    let muta_ok = run_prep(
        &ctx, &sys, sys_id, char_id, ship_id, 27, mod_id, &muta_fm, &muta_type, &rr_st, &fit, Some(target_id), true, false, false,
    ).await.unwrap();
    assert!((muta_ok.mutadaptive_spool_step - 0.15).abs() < 1e-6);
    assert!((muta_ok.mutadaptive_spool_max - 2.50).abs() < 1e-6);

    for (gid, check) in [(41u32, 1), (585, 2), (67, 3)] {
        let rt = synth_type(5000 + gid, gid, "Remote Assist", &[], &[]);
        let r_ok = run_prep(
            &ctx, &sys, sys_id, char_id, ship_id, 27, mod_id, &rr_fm, &rt, &rr_st, &fit, Some(target_id), false, false, false,
        ).await.unwrap();
        match check {
            1 => assert!((r_ok.remote_shield_amount - 200.0).abs() < 1e-6),
            2 => assert!((r_ok.remote_hull_amount - 150.0).abs() < 1e-6),
            _ => assert!((r_ok.remote_cap_amount - 120.0).abs() < 1e-6),
        }
    }

    // Disallow assistance check on target
    let _ = sys
        .add_module_cycle(crate::system_actor::ActiveModuleCycle {
            ship_id: target_id,
            module_id: 888_001,
            duration_s: 60.0,
            self_disallow_assistance: true,
            ..Default::default()
        })
        .await;
    assert_eq!(
        run_prep(&ctx, &sys, sys_id, char_id, ship_id, 27, mod_id, &muta_fm, &muta_type, &rr_st, &fit, Some(target_id), false, false, false)
            .await.err().unwrap().code,
        "TargetDisallowsAssistance"
    );

    // 7. Supercarrier Burst Projectors (group 842)
    for b_tid in [27678u32, 40636, 40635, 40696, 40697, 40698, 40699] {
        let (bfm, bst) = synth_module_and_state(mod_id, b_tid, 27, None, &[(2279, 20_000.0), (97, 1500.0), (20, -40.0)]);
        let btype = synth_type(b_tid, 842, "Burst Projector", &[], &[]);
        let bok = run_prep(
            &ctx, &sys, sys_id, char_id, ship_id, 659, mod_id, &bfm, &btype, &bst, &fit, Some(target_id), false, false, false,
        ).await.unwrap();
        if b_tid == 40698 {
            assert!((bok.target_velocity_multiplier.unwrap() - 0.60).abs() < 1e-6);
        }
    }
    assert!(sys.get_combat_state(target_id).await.unwrap().unwrap().capacitor < 5000.0);

    // 8. ewar_jamming_type and emit_committed_ewar_and_system_wide_notifications extra branches
    assert_eq!(ewar_jamming_type(&grap_type, None, false, None), Some("webify"));
    let gd_by_name = synth_type(37541, 291, "Guidance Disruptor II", &[], &[]);
    assert_eq!(ewar_jamming_type(&gd_by_name, None, false, None), Some("ewGuidanceDisrupt"));
    let non_ewar = synth_type(99999, 9999, "Passive Hull Mod", &[], &[]);
    assert_eq!(ewar_jamming_type(&non_ewar, None, false, None), None);
    emit_committed_ewar_and_system_wide_notifications(
        &ctx, sys_id, char_id, ship_id, mod_id, 99999, &non_ewar, None, false, None, Some(target_id),
    );
    let _ = AccountRecord {
        user_id: 1,
        username: String::new(),
        password_token: String::new(),
        role: 0,
        banned_until_unix: None,
        ban_reason: None,
    };
    let _ = PyRep::None;
}
